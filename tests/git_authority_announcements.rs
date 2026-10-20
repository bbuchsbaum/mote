use mote::git_authority::{
    client::Client,
    connection,
    coordination::{
        Action,
        announcements::{AnnouncedBegin, Announcement},
        communications,
        lifecycle::Begin,
    },
    history::Receipt,
    planner::SyntheticState,
    protocol::{Command, Effect, Manifest, OutcomeStatus, Request, Transaction, decode},
    transport::GitTransport,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command as ProcessCommand, Output, Stdio},
};
use tempfile::{TempDir, tempdir};

const PARENT: &str = "0000000000000000000000000000000000000000";

fn request(id: &str, action: Action) -> Request {
    Request {
        version: 1,
        request_id: id.into(),
        actor: "alice".into(),
        session: "sess-a".into(),
        timestamp: "2026-10-20T00:00:00Z".into(),
        command: Command::Coordination { action },
    }
}

fn apply(state: &mut SyntheticState, id: &str, action: Action) -> Transaction {
    let transaction = Transaction::prepare(state, PARENT, request(id, action)).unwrap();
    *state = transaction.validate(state, PARENT).unwrap();
    transaction
}

fn setup_task(state: &mut SyntheticState, task: &str) {
    apply(
        state,
        "session",
        Action::SessionStart {
            label: "worker".into(),
        },
    );
    apply(
        state,
        "task",
        Action::TaskCreate {
            id: task.into(),
            title: "task".into(),
            body: String::new(),
            priority: 2,
            assignee: None,
        },
    );
}

fn announced(
    task: &str,
    expect_status: &str,
    reservation: Option<&str>,
    paths: &[&str],
    note: Option<&str>,
    post: &str,
) -> Action {
    Action::AnnouncedBegin(AnnouncedBegin {
        begin: Begin {
            id: task.into(),
            expect_status: expect_status.into(),
            reservation: reservation.map(str::to_owned),
            paths: paths.iter().map(|path| (*path).into()).collect(),
            note: note.map(str::to_owned),
        },
        announcement: Announcement {
            id: post.into(),
            topic: "coordination".into(),
        },
    })
}

fn status_token(state: &SyntheticState, task: &str) -> String {
    state.coordination.task_view().unwrap()[task]["clock"]["status"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn announced_begin_is_a_single_replayable_receipt_with_ordered_effects() {
    let mut state = SyntheticState::default();
    setup_task(&mut state, "bd-task");

    let transaction = Transaction::prepare(
        &state,
        PARENT,
        request(
            "announce",
            announced(
                "bd-task",
                &status_token(&state, "bd-task"),
                None,
                &[],
                None,
                "post-claim",
            ),
        ),
    )
    .unwrap();
    assert_eq!(transaction.outcome.status, OutcomeStatus::Accepted);
    assert_eq!(transaction.effects.len(), 3);
    assert_eq!(
        transaction
            .effects
            .iter()
            .map(Effect::index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(matches!(transaction.effects[0], Effect::Ownership { .. }));
    assert!(matches!(transaction.effects[1], Effect::TaskOp { .. }));
    let Effect::CommunicationOp { op, .. } = &transaction.effects[2] else {
        panic!("announcement must be the final communication effect")
    };
    assert_eq!(op["kind"], "board_post");
    assert_eq!(op["reply_to"], Value::Null);
    assert_eq!(op["post_kind"], "post");
    assert!(op.get("answers").is_none());
    assert!(op.get("notify").is_none());
    assert!(op.get("idempotency_key").is_none());
    assert_eq!(
        transaction.outcome.result["announcement"],
        json!({"id":"post-claim","topic":"coordination"})
    );
    assert_eq!(
        transaction.outcome.result["effect_ids"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let replayed = transaction.validate(&state, PARENT).unwrap();
    assert_eq!(replayed.sequence, state.sequence + 1);
    assert_ne!(replayed.coordination, state.coordination);
}

#[test]
fn announced_begin_with_reservation_and_note_keeps_global_effect_indices() {
    let mut state = SyntheticState::default();
    setup_task(&mut state, "bd-task");
    let expect_status = status_token(&state, "bd-task");
    let transaction = apply(
        &mut state,
        "reserve-announce",
        announced(
            "bd-task",
            &expect_status,
            Some("rv-paths"),
            &["src/a.rs", "src/b.rs"],
            Some("starting"),
            "post-reserve",
        ),
    );
    assert_eq!(transaction.outcome.status, OutcomeStatus::Accepted);
    assert_eq!(transaction.effects.len(), 5);
    assert_eq!(
        transaction
            .effects
            .iter()
            .map(Effect::index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4]
    );
    let Effect::CommunicationOp { op, .. } = &transaction.effects[4] else {
        panic!("final effect must announce")
    };
    assert_eq!(
        op["body"],
        "claiming bd-task for rv-paths on src/a.rs, src/b.rs"
    );
    let view = state.coordination.view().unwrap();
    assert_eq!(view["tasks"]["bd-task"]["status"], "doing");
    assert_eq!(
        view["reservations"]["rv-paths"]["paths"],
        json!(["src/a.rs", "src/b.rs"])
    );
}

#[test]
fn late_duplicate_announcement_rejects_without_leaking_provisional_begin() {
    let mut state = SyntheticState::default();
    setup_task(&mut state, "bd-task");
    apply(
        &mut state,
        "existing-post",
        Action::Communication {
            action: communications::Action::Post {
                id: "post-duplicate".into(),
                topic: "coordination".into(),
                body: "already here".into(),
                reply_to: None,
                post_kind: "post".into(),
                answers: vec![],
                notify: vec![],
                idempotency_key: None,
            },
        },
    );
    let before = state.coordination.clone();
    let transaction = Transaction::prepare(
        &state,
        PARENT,
        request(
            "duplicate",
            announced(
                "bd-task",
                &status_token(&state, "bd-task"),
                None,
                &[],
                None,
                "post-duplicate",
            ),
        ),
    )
    .unwrap();
    assert_eq!(transaction.outcome.status, OutcomeStatus::Rejected);
    assert!(transaction.effects.is_empty());
    assert_eq!(
        transaction.outcome.result["code"],
        "communication_effect_rejected"
    );
    assert_eq!(transaction.outcome.result["index"], 2);
    let after = transaction.validate(&state, PARENT).unwrap();
    assert_eq!(after.coordination, before);
    assert_eq!(
        after.sequence,
        state.sequence + 1,
        "the rejected outer receipt is durable"
    );
}

#[test]
fn only_announcement_profiles_accept_the_new_action_and_wire_is_strict() {
    let action = Command::Coordination {
        action: announced("bd-task", "status", None, &[], None, "post-claim"),
    };
    let legacy = [
        Manifest::new("st-test".into()),
        Manifest::coordination("st-test".into()),
        Manifest::lifecycle("st-test".into()),
        Manifest::migrated("st-test".into(), "0".repeat(64)),
        Manifest::migrated_recoverable("st-test".into(), "0".repeat(64)),
        Manifest::communications("st-test".into()),
        Manifest::migrated_communications("st-test".into(), "0".repeat(64)),
    ];
    assert!(
        legacy
            .iter()
            .all(|manifest| manifest.allows(&action).is_err())
    );
    assert!(
        Manifest::announcements("st-test".into())
            .allows(&action)
            .is_ok()
    );
    assert!(
        Manifest::migrated_announcements("st-test".into(), "0".repeat(64))
            .allows(&action)
            .is_ok()
    );

    let valid = json!({"kind":"begin.announce","begin":{"id":"bd-task","expect_status":"eff-task-000000","reservation":null,"paths":[],"note":null},"announcement":{"id":"post-claim","topic":"coordination"}});
    let mut unknown = valid.clone();
    unknown["announcement"]["extra"] = json!(true);
    assert!(serde_json::from_value::<Action>(unknown).is_err());
    let mut missing_nullable = valid.clone();
    missing_nullable["begin"]
        .as_object_mut()
        .unwrap()
        .remove("note");
    assert!(serde_json::from_value::<Action>(missing_nullable).is_err());
    let mut invalid_topic = valid;
    invalid_topic["announcement"]["topic"] = json!(" bad");
    let invalid_action = serde_json::from_value::<Action>(invalid_topic).unwrap();
    assert!(request("bad-topic", invalid_action).validate().is_err());
}

#[test]
fn announcement_body_overflow_is_rejected_before_any_lifecycle_planning() {
    let paths = (0..64)
        .map(|index| format!("src/{index:02}-{}", "x".repeat(20_000)))
        .collect::<Vec<_>>();
    let action = Action::AnnouncedBegin(AnnouncedBegin {
        begin: Begin {
            id: "bd-task".into(),
            expect_status: "token".into(),
            reservation: Some("rv-paths".into()),
            paths,
            note: None,
        },
        announcement: Announcement {
            id: "post-overflow".into(),
            topic: "coordination".into(),
        },
    });
    assert!(request("overflow", action).validate().is_err());
}

#[test]
fn already_doing_task_announces_a_reacquired_claim_without_another_status_patch() {
    let mut state = SyntheticState::default();
    setup_task(&mut state, "bd-task");
    let expect_status = status_token(&state, "bd-task");
    let opened = apply(
        &mut state,
        "ordinary-begin",
        Action::Begin(Begin {
            id: "bd-task".into(),
            expect_status,
            reservation: None,
            paths: vec![],
            note: None,
        }),
    );
    let claim = state.coordination.view().unwrap()["claims"]["bd-task"]["token"]
        .as_str()
        .unwrap()
        .to_owned();
    apply(
        &mut state,
        "release",
        Action::ClaimRelease {
            id: "bd-task".into(),
            token: claim,
        },
    );
    assert_eq!(opened.effects.len(), 2);
    let transaction = Transaction::prepare(
        &state,
        PARENT,
        request(
            "reannounce",
            announced(
                "bd-task",
                &status_token(&state, "bd-task"),
                None,
                &[],
                None,
                "post-reclaim",
            ),
        ),
    )
    .unwrap();
    assert_eq!(transaction.outcome.status, OutcomeStatus::Accepted);
    assert_eq!(transaction.effects.len(), 2);
    assert!(matches!(transaction.effects[0], Effect::Ownership { .. }));
    assert!(matches!(
        transaction.effects[1],
        Effect::CommunicationOp { .. }
    ));
    assert_eq!(transaction.effects[0].index(), 0);
    assert_eq!(transaction.effects[1].index(), 1);
}

struct CliFixture {
    root: TempDir,
}

impl CliFixture {
    fn new() -> Self {
        let f = Self {
            root: tempdir().unwrap(),
        };
        let remote = f.root.path().join("remote.git");
        git(
            f.root.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        for name in ["a", "b"] {
            let repo = f.root.path().join(name);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
            mote::Store::init(&repo).unwrap();
        }
        f.ok("a", "alice", None, &["authority", "init-git"]);
        f.ok("b", "bob", None, &["authority", "connect"]);
        f
    }

    fn repo(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join(name)
    }
    fn command(
        &self,
        name: &str,
        actor: &str,
        session: Option<&str>,
        args: &[&str],
    ) -> ProcessCommand {
        let repo = self.repo(name);
        let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_mote"));
        isolate(&mut command);
        command
            .current_dir(&repo)
            .arg("--store")
            .arg(repo.join(".mote"))
            .args(["--actor", actor, "--json"])
            .args(args);
        if let Some(session) = session {
            command.env("MOTE_SESSION", session);
        }
        command
    }
    fn run(&self, name: &str, actor: &str, session: Option<&str>, args: &[&str]) -> Output {
        self.command(name, actor, session, args).output().unwrap()
    }
    fn ok(&self, name: &str, actor: &str, session: Option<&str>, args: &[&str]) -> Value {
        decode_cli(self.run(name, actor, session, args), 0)
    }
    fn start(&self, name: &str, actor: &str) -> String {
        self.ok(name, actor, None, &["session", "start", "--as", actor])["session_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn transport(&self, name: &str, label: &str) -> GitTransport {
        GitTransport::open(
            &self.repo(name),
            "origin",
            "mote-state",
            &self.root.path().join("scratch").join(label),
        )
        .unwrap()
    }
    fn no_local_ops(&self) {
        for name in ["a", "b"] {
            assert_eq!(
                fs::read_dir(self.repo(name).join(".mote/ops"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}

fn isolate(command: &mut ProcessCommand) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") || name.to_string_lossy().starts_with("MOTE_")
        {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
}
fn git(repo: &Path, args: &[&str]) {
    let mut command = ProcessCommand::new("git");
    isolate(&mut command);
    let output = command.arg("-C").arg(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn decode_cli(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cli_announcement_race_retry_and_cache_loss_preserve_one_canonical_receipt() {
    let f = CliFixture::new();
    let alice = f.start("a", "alice");
    let bob = f.start("b", "bob");
    let task = f.ok("a", "alice", Some(&alice), &["new", "shared task"])["receipt"]["outcome"]["result"]["subject"].as_str().unwrap().to_owned();
    let unused = f.ok("a", "alice", Some(&alice), &["new", "unused task"])["receipt"]
        ["outcome"]["result"]["subject"]
        .as_str()
        .unwrap()
        .to_owned();
    let before_invalid = f.transport("a", "before-invalid").head().unwrap();
    let invalid = f.run(
        "a",
        "alice",
        Some(&alice),
        &["begin", &unused, "--announce", "bad\ntopic"],
    );
    assert!(!invalid.status.success());
    assert_eq!(
        f.transport("a", "after-invalid").head().unwrap(),
        before_invalid
    );
    let mut left = f
        .command(
            "a",
            "alice",
            Some(&alice),
            &[
                "begin",
                &task,
                "--paths",
                "src/shared.rs",
                "--announce",
                "design",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let right = f
        .command(
            "b",
            "bob",
            Some(&bob),
            &[
                "begin",
                &task,
                "--paths",
                "src/shared.rs",
                "--announce",
                "design",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let right = match right {
        Ok(child) => child,
        Err(error) => {
            let _ = left.kill();
            let _ = left.wait();
            panic!("second child spawn: {error}")
        }
    };
    let first_result = left.wait_with_output();
    let second_result = right.wait_with_output();
    let first = first_result.unwrap();
    let second = second_result.unwrap();
    let mut codes = [first.status.code().unwrap(), second.status.code().unwrap()];
    codes.sort();
    assert_eq!(codes, [0, 2]);
    let (winner_name, winner, loser) = if first.status.code() == Some(0) {
        (
            "a",
            serde_json::from_slice::<Value>(&first.stdout).unwrap(),
            &second,
        )
    } else {
        (
            "b",
            serde_json::from_slice::<Value>(&second.stdout).unwrap(),
            &first,
        )
    };
    assert_eq!(
        loser.status.code(),
        Some(2),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&loser.stdout),
        String::from_utf8_lossy(&loser.stderr)
    );
    let loser: Value = serde_json::from_slice(&loser.stdout).unwrap();

    assert_eq!(winner["status"], "accepted");
    assert_eq!(loser["status"], "rejected");
    let receipt: Receipt = serde_json::from_value(winner["receipt"].clone()).unwrap();
    let loser_receipt: Receipt = serde_json::from_value(loser["receipt"].clone()).unwrap();
    let transaction_for = |receipt: &Receipt| {
        let path = format!(
            "{}:transactions/{:020}.json",
            receipt.commit, receipt.sequence
        );
        let mut show = ProcessCommand::new("git");
        isolate(&mut show);
        let output = show
            .arg("--git-dir")
            .arg(f.root.path().join("remote.git"))
            .args(["show", &path])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git show {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        decode::<Transaction>(&output.stdout).unwrap()
    };
    let canonical = transaction_for(&receipt);
    let loser_canonical = transaction_for(&loser_receipt);
    assert_eq!(canonical.outcome, receipt.outcome);
    assert_eq!(loser_canonical.outcome, loser_receipt.outcome);
    assert_eq!(canonical.outcome.status, OutcomeStatus::Accepted);
    assert_eq!(loser_canonical.outcome.status, OutcomeStatus::Rejected);
    let original = canonical.request.clone();
    let before_head = f.transport("a", "before-retry").head().unwrap().unwrap();
    let retry = f.ok(
        winner_name,
        if winner_name == "a" { "alice" } else { "bob" },
        None,
        &["authority", "retry", &original.request_id],
    );
    assert_eq!(retry["receipt"], serde_json::to_value(&receipt).unwrap());
    assert_eq!(
        f.transport("a", "after-retry").head().unwrap().unwrap(),
        before_head
    );

    let winner_store = mote::Store::open(&f.repo(winner_name).join(".mote")).unwrap();
    let cache = connection::default_cache_root(&winner_store);
    fs::remove_dir_all(&cache).unwrap();
    let replacement = Client::connect(
        f.transport(winner_name, "replacement"),
        &f.root.path().join("replacement-cache"),
    )
    .unwrap();
    let recovered = replacement.submit(original, 8).unwrap();
    assert!(recovered.recovered);
    assert_eq!(recovered.receipt.as_ref(), Some(&receipt));
    assert_eq!(replacement.refresh().unwrap().head, before_head);
    let view = replacement
        .refresh()
        .unwrap()
        .state
        .coordination
        .view()
        .unwrap();
    let claim = &view["claims"][&task];
    assert_eq!(
        claim["session"],
        if winner_name == "a" {
            alice.as_str()
        } else {
            bob.as_str()
        }
    );
    assert_eq!(
        claim["token"],
        winner["receipt"]["outcome"]["result"]["claim_token"]
    );
    let reservation = &winner["receipt"]["outcome"]["result"]["reservation"];
    assert_eq!(
        view["reservations"][reservation["id"].as_str().unwrap()]["token"],
        reservation["token"]
    );
    assert_eq!(
        view["reservations"][reservation["id"].as_str().unwrap()]["paths"],
        reservation["paths"]
    );
    let announcement = &winner["receipt"]["outcome"]["result"]["announcement"];
    let post = &view["communications"]["board_posts"][announcement["id"].as_str().unwrap()];
    assert_eq!(post["topic"], announcement["topic"]);
    assert_eq!(
        post["body"],
        format!(
            "claiming {task} for {} on src/shared.rs",
            reservation["id"].as_str().unwrap()
        )
    );
    f.no_local_ops();
}
