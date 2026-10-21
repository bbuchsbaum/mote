//! Real-Git acceptance witnesses for lifecycle ownership races.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier},
    thread,
};

use mote::git_authority::{
    client::{Client, Submission, SubmissionStatus},
    protocol::{Manifest, Request, Transaction, decode, encode, transaction_path},
    transport::GitTransport,
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

const CHILD: &str = "MOTE_GA_LIFECYCLE_ACCEPTANCE_CHILD";

fn isolate(command: &mut Command) {
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

// API transports inherit this process environment. Run each API witness in
// its own scrubbed child instead of changing process-global variables while
// Rust's test runner executes other tests concurrently.
fn isolated(test: &str) -> bool {
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return false;
    }
    let mut command = Command::new(std::env::current_exe().unwrap());
    isolate(&mut command);
    let output = command
        .args(["--exact", test, "--nocapture"])
        .env(CHILD, test)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated {test}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    true
}

fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/git_authority/lifecycle.json")).unwrap()
}

fn request(id: &str, actor: &str, session: &str, action: Value) -> Request {
    serde_json::from_value(json!({
        "version": 1, "request_id": id, "actor": actor, "session": session,
        "timestamp": "2026-10-20T00:00:00Z",
        "command": {"kind": "coordination", "action": action}
    }))
    .unwrap()
}

fn accepted(client: &Client, request: Request) -> Submission {
    let outcome = client.submit(request, 12).unwrap();
    assert_eq!(outcome.status, SubmissionStatus::Accepted, "{outcome:?}");
    outcome
}

struct Fixture {
    td: TempDir,
}

impl Fixture {
    fn new() -> Self {
        assert!(
            std::env::var_os(CHILD).is_some(),
            "API fixtures require a scrubbed child"
        );
        // Check before any filesystem setup, including for a manually invoked
        // child with a forged marker and inherited selectors/config injection.
        for (name, _) in std::env::vars_os() {
            let name = name.to_string_lossy();
            if name.starts_with("GIT_") {
                assert!(
                    matches!(
                        name.as_ref(),
                        "GIT_CONFIG_NOSYSTEM" | "GIT_CONFIG_GLOBAL" | "GIT_TERMINAL_PROMPT"
                    ),
                    "unexpected inherited Git variable: {name}"
                );
            }
            if name.starts_with("MOTE_") {
                assert_eq!(name, CHILD);
            }
        }
        assert_eq!(std::env::var("GIT_CONFIG_NOSYSTEM").unwrap(), "1");
        assert_eq!(std::env::var("GIT_CONFIG_GLOBAL").unwrap(), "/dev/null");
        assert_eq!(std::env::var("GIT_TERMINAL_PROMPT").unwrap(), "0");
        let f = Self {
            td: tempdir().unwrap(),
        };
        let remote = f.td.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q"]);
        for name in ["a", "b", "c"] {
            let repo = f.td.path().join(name);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
        }
        let transport = f.transport("a", "root");
        let root = transport
            .create_commit(
                None,
                &BTreeMap::from([(
                    "manifest.json".into(),
                    encode(&Manifest::announcements("st-lifecycle-acceptance".into())).unwrap(),
                )]),
            )
            .unwrap();
        assert!(transport.push(&root).unwrap().success);
        f
    }

    fn transport(&self, repo: &str, name: &str) -> GitTransport {
        GitTransport::open(
            &self.td.path().join(repo),
            "origin",
            "mote-state",
            &self.td.path().join("state").join(name).join("git"),
        )
        .unwrap()
    }

    fn client(&self, repo: &str, name: &str) -> Client {
        Client::connect(
            self.transport(repo, name),
            &self.td.path().join("state").join(name).join("cache"),
        )
        .unwrap()
    }

    fn seed(&self, client: &Client, scenario: usize) {
        for seed in vectors()["scenarios"][scenario]["seed"].as_array().unwrap() {
            accepted(client, serde_json::from_value(seed.clone()).unwrap());
        }
    }
}

fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    isolate(&mut command);
    command.arg("-C").arg(repo).args(args);
    assert!(command.status().unwrap().success());
}

fn race(
    left: Client,
    left_request: Request,
    right: Client,
    right_request: Request,
) -> (Submission, Submission) {
    let gate = Arc::new(Barrier::new(2));
    let other = Arc::clone(&gate);
    let left = thread::spawn(move || {
        other.wait();
        left.submit(left_request, 4).unwrap()
    });
    let right = thread::spawn(move || {
        gate.wait();
        right.submit(right_request, 4).unwrap()
    });
    (left.join().unwrap(), right.join().unwrap())
}

#[test]
fn one_eligible_task_races_to_one_owner_and_retry_is_receipt_stable() {
    if isolated("one_eligible_task_races_to_one_owner_and_retry_is_receipt_stable") {
        return;
    }
    let f = Fixture::new();
    let seeder = f.client("a", "seed");
    f.seed(&seeder, 0);
    let next = |id, actor, session| {
        request(
            id,
            actor,
            session,
            json!({
                "kind":"next", "tags":[], "exclude":[], "reservation":"rv-next", "paths":["src/next.rs"], "note":"claim next"
            }),
        )
    };
    let winner_request = next("next-a", "alice", "sess-a");
    let competing_request = next("next-b", "bob", "sess-b");
    let (a, b) = race(
        f.client("a", "next-a"),
        winner_request.clone(),
        f.client("b", "next-b"),
        competing_request.clone(),
    );
    let (winner, retry_request, loser) = if a.status == SubmissionStatus::Accepted {
        (a, winner_request, b)
    } else {
        (b, competing_request, a)
    };
    assert_eq!(winner.status, SubmissionStatus::Accepted);
    assert_eq!(loser.status, SubmissionStatus::Rejected);
    assert_eq!(
        loser.receipt.as_ref().unwrap().outcome.result["code"],
        "no_ready_task"
    );
    let before = f.client("c", "retry-view").refresh().unwrap().head;
    let retry = f.client("c", "retry").submit(retry_request, 12).unwrap();
    assert!(retry.recovered);
    assert_eq!(retry.receipt, winner.receipt);
    assert_eq!(f.client("c", "after").refresh().unwrap().head, before);
}

fn assert_competing_lifecycle_pair(label: &str, left_action: &str, right_action: &str) {
    let f = Fixture::new();
    let seed = f.client("a", &format!("seed-{label}"));
    f.seed(&seed, 1);
    let begin: Request =
        serde_json::from_value(vectors()["scenarios"][1]["vectors"][0]["request"].clone()).unwrap();
    let begin = accepted(&seed, begin).receipt.unwrap();
    let clock =
            seed.refresh().unwrap().state.coordination.view().unwrap()["tasks"]["bd-one"]["clock"]
                ["status"]
                .as_str()
                .unwrap()
                .to_owned();
    let claim = begin.outcome.result["claim_token"].clone();
    let action = |kind: &str| match kind {
        "done" => {
            json!({"kind":"done","id":"bd-one","expect_status":clock,"expect_claim":claim,"reservations":{"rv-one":"eff-begin-work-000000"},"note":"complete"})
        }
        "handoff" => {
            json!({"kind":"handoff","id":"bd-one","expect_status":clock,"expect_claim":claim,"reservations":{"rv-one":"eff-begin-work-000000"},"to_session":"sess-b","disposition":"transfer","note":"handoff"})
        }
        "release" => json!({"kind":"claim.release","id":"bd-one","token":claim}),
        _ => unreachable!(),
    };
    let (a, b) = race(
        f.client("a", &format!("{label}-a")),
        request(
            &format!("{label}-a"),
            "alice",
            "sess-a",
            action(left_action),
        ),
        f.client("b", &format!("{label}-b")),
        request(
            &format!("{label}-b"),
            "alice",
            "sess-a",
            action(right_action),
        ),
    );
    assert_eq!(
        [a.status, b.status]
            .into_iter()
            .filter(|s| *s == SubmissionStatus::Accepted)
            .count(),
        1,
        "{label}: {a:?} {b:?}"
    );
    let (winner, winner_action) = if a.status == SubmissionStatus::Accepted {
        (&a, left_action)
    } else {
        (&b, right_action)
    };
    let loser = if a.status == SubmissionStatus::Rejected {
        &a
    } else {
        &b
    };
    assert_eq!(loser.status, SubmissionStatus::Rejected);
    let left = f.client("a", &format!("{label}-view-a")).refresh().unwrap();
    let right = f.client("b", &format!("{label}-view-b")).refresh().unwrap();
    assert_eq!(left.head, right.head, "{label}");
    let losing_receipt = loser.receipt.as_ref().unwrap();
    let losing_transaction: Transaction = decode(
        left.files
            .get(&transaction_path(losing_receipt.sequence))
            .unwrap(),
    )
    .unwrap();
    assert!(
        losing_transaction.effects.is_empty(),
        "{label}: loser effects"
    );
    let view = left.state.coordination.view().unwrap();
    match winner_action {
        "done" => {
            assert_eq!(view["tasks"]["bd-one"]["status"], "closed");
            assert!(view["claims"].get("bd-one").is_none());
            assert_eq!(view["reservations"]["rv-one"]["status"], "closed");
        }
        "handoff" => {
            assert_eq!(view["claims"]["bd-one"]["actor"], "bob");
            assert_eq!(view["claims"]["bd-one"]["session"], "sess-b");
            assert_eq!(
                view["claims"]["bd-one"]["token"],
                winner.receipt.as_ref().unwrap().outcome.result["claim_token"]
            );
            assert_eq!(view["reservations"]["rv-one"]["actor"], "bob");
            assert_eq!(view["reservations"]["rv-one"]["session"], "sess-b");
            assert_eq!(
                view["reservations"]["rv-one"]["token"],
                winner.receipt.as_ref().unwrap().outcome.result["reservations"][0]["token"]
            );
        }
        "release" => {
            assert!(view["claims"].get("bd-one").is_none());
            assert_eq!(view["reservations"]["rv-one"]["status"], "open");
            assert_eq!(view["reservations"]["rv-one"]["actor"], "alice");
        }
        _ => unreachable!(),
    }
    assert_eq!(
        left.state.coordination.view().unwrap(),
        right.state.coordination.view().unwrap()
    );
}

#[test]
fn competing_done_and_handoff_have_one_winner_without_partial_effects() {
    if isolated("competing_done_and_handoff_have_one_winner_without_partial_effects") {
        return;
    }
    assert_competing_lifecycle_pair("done-handoff", "done", "handoff");
}

#[test]
fn competing_done_and_release_have_one_winner_without_partial_effects() {
    if isolated("competing_done_and_release_have_one_winner_without_partial_effects") {
        return;
    }
    assert_competing_lifecycle_pair("done-release", "done", "release");
}

#[test]
fn competing_handoff_and_release_have_one_winner_without_partial_effects() {
    if isolated("competing_handoff_and_release_have_one_winner_without_partial_effects") {
        return;
    }
    assert_competing_lifecycle_pair("handoff-release", "handoff", "release");
}

#[test]
fn done_retry_keeps_the_original_receipt_head_and_single_completion_note() {
    if isolated("done_retry_keeps_the_original_receipt_head_and_single_completion_note") {
        return;
    }
    let f = Fixture::new();
    let client = f.client("a", "done-seed");
    f.seed(&client, 1);
    let begin: Request =
        serde_json::from_value(vectors()["scenarios"][1]["vectors"][0]["request"].clone()).unwrap();
    let begin = accepted(&client, begin).receipt.unwrap();
    let clock = client.refresh().unwrap().state.coordination.view().unwrap()["tasks"]["bd-one"]
        ["clock"]["status"]
        .clone();
    let done = request(
        "done-retry",
        "alice",
        "sess-a",
        json!({
            "kind":"done", "id":"bd-one", "expect_status":clock,
            "expect_claim":begin.outcome.result["claim_token"],
            "reservations":{"rv-one":"eff-begin-work-000000"}, "note":"complete once"
        }),
    );
    let first = accepted(&client, done.clone());
    let head = client.refresh().unwrap().head;
    let retry = client.submit(done, 12).unwrap();
    assert!(retry.recovered);
    assert_eq!(retry.receipt, first.receipt);
    let snapshot = client.refresh().unwrap();
    assert_eq!(snapshot.head, head);
    let view = snapshot.state.coordination.view().unwrap();
    let notes = view["tasks"]["bd-one"]["notes"].as_array().unwrap();
    assert_eq!(
        notes
            .iter()
            .filter(|note| note["text"] == "complete once")
            .count(),
        1
    );
}

#[test]
fn concurrent_recovery_invalidates_the_source_and_fresh_client_recovers_winner_receipt() {
    if isolated(
        "concurrent_recovery_invalidates_the_source_and_fresh_client_recovers_winner_receipt",
    ) {
        return;
    }
    let f = Fixture::new();
    let seed = f.client("a", "seed");
    f.seed(&seed, 2);
    let recover: Request =
        serde_json::from_value(vectors()["scenarios"][2]["vectors"][0]["request"].clone()).unwrap();
    let mut competing_value = serde_json::to_value(&recover).unwrap();
    competing_value["request_id"] = json!("recover-a-competing");
    let competing: Request = serde_json::from_value(competing_value).unwrap();
    let (a, b) = race(
        f.client("b", "recover-a"),
        recover.clone(),
        f.client("c", "recover-b"),
        competing.clone(),
    );
    assert_eq!(
        [a.status, b.status]
            .into_iter()
            .filter(|status| *status == SubmissionStatus::Accepted)
            .count(),
        1
    );
    let (winner, winner_request, loser, winner_cache) = if a.status == SubmissionStatus::Accepted {
        (a, recover, b, "recover-a")
    } else {
        (b, competing, a, "recover-b")
    };
    assert_eq!(winner.status, SubmissionStatus::Accepted);
    assert_eq!(loser.status, SubmissionStatus::Rejected);
    assert_eq!(
        loser.receipt.as_ref().unwrap().outcome.result["code"],
        "session_unavailable"
    );
    fs::remove_dir_all(f.td.path().join("state").join(winner_cache)).unwrap();
    let fresh = f
        .client("c", "fresh-recover")
        .submit(winner_request, 12)
        .unwrap();
    assert!(fresh.recovered);
    assert_eq!(fresh.receipt, winner.receipt);
    let snapshot = f.client("c", "recover-view").refresh().unwrap();
    let view = snapshot.state.coordination.view().unwrap();
    assert_eq!(view["sessions"]["sess-a"]["status"], "revoked");
    assert_eq!(view["claims"]["bd-one"]["actor"], "bob");
    assert_eq!(view["claims"]["bd-one"]["session"], "sess-b");
    assert_eq!(view["reservations"]["rv-one"]["actor"], "bob");
    assert_eq!(view["reservations"]["rv-one"]["session"], "sess-b");
    let winning: Transaction = decode(
        snapshot
            .files
            .get(&transaction_path(winner.receipt.as_ref().unwrap().sequence))
            .unwrap(),
    )
    .unwrap();
    let effects = serde_json::to_value(winning).unwrap()["effects"]
        .as_array()
        .unwrap()
        .clone();
    let claim = effects
        .iter()
        .find(|effect| effect["transition"]["kind"] == "claim_recovered")
        .unwrap();
    let reservation = effects
        .iter()
        .find(|effect| effect["transition"]["kind"] == "reservation_recovered")
        .unwrap();
    assert_eq!(
        view["claims"]["bd-one"]["token"],
        claim["transition"]["new_token"]
    );
    assert_eq!(
        view["reservations"]["rv-one"]["token"],
        reservation["transition"]["new_token"]
    );
    let losing: Transaction = decode(
        snapshot
            .files
            .get(&transaction_path(loser.receipt.as_ref().unwrap().sequence))
            .unwrap(),
    )
    .unwrap();
    assert!(losing.effects.is_empty());
}

#[test]
fn same_actor_sessions_release_only_their_own_work() {
    if isolated("same_actor_sessions_release_only_their_own_work") {
        return;
    }
    let f = Fixture::new();
    let a = f.client("a", "same-seed");
    for (id, session) in [("sess-one", "one"), ("sess-two", "two")] {
        accepted(
            &a,
            request(
                id,
                "alice",
                id,
                json!({"kind":"session.start","label":session}),
            ),
        );
    }
    for (id, priority) in [("bd-one", 1), ("bd-two", 2)] {
        accepted(
            &a,
            request(
                &format!("create-{id}"),
                "alice",
                "sess-one",
                json!({"kind":"task.create","id":id,"title":id,"body":"","priority":priority,"assignee":null}),
            ),
        );
    }
    let snapshot = a.refresh().unwrap();
    for (task, session, rv, path) in [
        ("bd-one", "sess-one", "rv-one", "src/one.rs"),
        ("bd-two", "sess-two", "rv-two", "src/two.rs"),
    ] {
        let clock =
            snapshot.state.coordination.view().unwrap()["tasks"][task]["clock"]["status"].clone();
        accepted(
            &a,
            request(
                &format!("begin-{task}"),
                "alice",
                session,
                json!({"kind":"begin","id":task,"expect_status":clock,"reservation":rv,"paths":[path],"note":"working"}),
            ),
        );
    }
    accepted(
        &a,
        request(
            "end-one",
            "alice",
            "sess-one",
            json!({"kind":"session.end","expect_session":"eff-sess-one-000000","reason":"one finished"}),
        ),
    );
    let view = a.refresh().unwrap().state.coordination.view().unwrap();
    assert!(view["claims"].get("bd-one").is_none());
    assert_eq!(view["reservations"]["rv-one"]["status"], "closed");
    assert_eq!(view["claims"]["bd-two"]["session"], "sess-two");
    assert_eq!(view["reservations"]["rv-two"]["status"], "open");
}

fn inventory(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &entry.path(), files);
            } else {
                files.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn api_witnesses_scrub_inherited_selectors_without_touching_an_unrelated_repository() {
    let unrelated = tempdir().unwrap();
    git(unrelated.path(), &["init", "-q"]);
    fs::write(unrelated.path().join("dirty.txt"), b"unrelated bytes\n").unwrap();
    let before = inventory(unrelated.path());
    let mut command = Command::new(std::env::current_exe().unwrap());
    isolate(&mut command);
    let output = command
        .args([
            "--exact",
            "one_eligible_task_races_to_one_owner_and_retry_is_receipt_stable",
            "--nocapture",
        ])
        .env("GIT_DIR", unrelated.path().join(".git"))
        .env("GIT_WORK_TREE", unrelated.path())
        .env("GIT_COMMON_DIR", unrelated.path().join(".git"))
        .env(
            "GIT_OBJECT_DIRECTORY",
            unrelated.path().join(".git/objects"),
        )
        .env("GIT_INDEX_FILE", unrelated.path().join("foreign-index"))
        .env("GIT_NAMESPACE", "foreign-namespace")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.bare")
        .env("GIT_CONFIG_VALUE_0", "true")
        .env("MOTE_STORE", unrelated.path().join("foreign-store"))
        .env("MOTE_ACTOR", "foreign-actor")
        .env("MOTE_SESSION", "foreign-session")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(inventory(unrelated.path()), before);
}
