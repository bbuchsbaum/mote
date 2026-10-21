use mote::{
    Store,
    git_authority::{
        connection,
        coordination::{
            Action,
            lifecycle::{Disposition, SessionRecover},
        },
        protocol::{Command as ProtocolCommand, Request},
        transport::GitTransport,
    },
};
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    td: TempDir,
    a: Store,
    b: Store,
}
impl Fixture {
    fn new() -> Self {
        let td = tempdir().unwrap();
        let remote = td.path().join("remote.git");
        git(
            td.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        for name in ["a", "b"] {
            let repo = td.path().join(name);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
        }
        Self {
            a: Store::init(&td.path().join("a")).unwrap(),
            b: Store::init(&td.path().join("b")).unwrap(),
            td,
        }
    }
    fn repo(&self, name: &str) -> std::path::PathBuf {
        self.td.path().join(name)
    }
    fn store(&self, name: &str) -> &Store {
        if name == "a" { &self.a } else { &self.b }
    }
    fn run(&self, name: &str, actor: &str, session: Option<&str>, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
        command
            .current_dir(self.repo(name))
            .env_remove("MOTE_SESSION")
            .env_remove("MOTE_ACTOR")
            .env_remove("MOTE_STORE")
            .arg("--store")
            .arg(self.store(name).root())
            .arg("--actor")
            .arg(actor)
            .args(args);
        if let Some(session) = session {
            command.env("MOTE_SESSION", session);
        }
        command.output().unwrap()
    }
    fn start(&self, name: &str, actor: &str) -> String {
        let out = self.run(
            name,
            actor,
            None,
            &["--json", "session", "start", "--as", actor],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["session_id"]
            .as_str()
            .unwrap()
            .into()
    }
}

fn json_ok(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
fn assert_provenance(value: &Value, fresh: bool) {
    assert_eq!(value["authority"]["fresh"], fresh);
    assert_eq!(value["authority"]["ownership"], "explicit-release-v1");
    assert!(value["authority"]["observed_at"].is_string());
    assert!(value["authority"]["head"].is_string());
    assert!(value["authority"]["root"].is_string());
    assert!(value["authority"]["store_id"].is_string());
    assert!(value["authority"]["generation"].is_number());
    assert!(value["authority"]["sequence"].is_number());
}

#[test]
fn cli_handoff_recovery_and_cached_reads_keep_remote_ownership_and_provenance() {
    let f = Fixture::new();
    json_ok(f.run("a", "alice", None, &["--json", "authority", "init-git"]));
    json_ok(f.run("b", "bob", None, &["--json", "authority", "connect"]));
    let alice = f.start("a", "alice");
    let bob = f.start("b", "bob");
    let created = json_ok(f.run("a", "alice", Some(&alice), &["--json", "new", "shared"]));
    let id = created["receipt"]["outcome"]["result"]["subject"]
        .as_str()
        .unwrap();
    assert_provenance(
        &json_ok(f.run("a", "alice", None, &["--json", "show", id])),
        true,
    );
    assert_provenance(&json_ok(f.run("a", "alice", None, &["--json", "ls"])), true);
    let request_id = created["request_id"].as_str().unwrap();
    let receipt = json_ok(f.run(
        "a",
        "alice",
        None,
        &["--json", "authority", "receipt", request_id],
    ));
    assert_provenance(&receipt, true);
    assert!(receipt["data"]["receipt"].is_object());
    let absent = f.run(
        "a",
        "alice",
        None,
        &["--json", "authority", "receipt", "not-submitted"],
    );
    assert_eq!(absent.status.code(), Some(2));
    let absent: Value = serde_json::from_slice(&absent.stdout).unwrap();
    assert_provenance(&absent, true);
    assert!(absent["data"]["receipt"].is_null());
    let ready = json_ok(f.run("b", "bob", None, &["--json", "ready"]));
    assert_eq!(ready["data"][0]["id"], id);
    let next = json_ok(f.run("a", "alice", Some(&alice), &["--json", "next"]));
    assert_eq!(next["receipt"]["outcome"]["result"]["task"], id);
    let reserved = json_ok(f.run(
        "a",
        "alice",
        Some(&alice),
        &["--json", "reserve", "--issue", id, "src/z.rs", "src/a.rs"],
    ));
    let rv = reserved["receipt"]["outcome"]["result"]["subject"]
        .as_str()
        .unwrap();
    let preflight = f.run(
        "b",
        "bob",
        Some(&bob),
        &["--json", "preflight", "--issue", id, "--paths", "src/"],
    );
    assert_eq!(preflight.status.code(), Some(2));
    let held = json_ok(f.run("b", "bob", None, &["--json", "who-has", "src/a.rs"]));
    assert_eq!(held["data"][rv]["session"], alice);
    json_ok(f.run(
        "a",
        "alice",
        Some(&alice),
        &[
            "--json",
            "handoff",
            id,
            "--to",
            "bob",
            "--note",
            "your turn",
        ],
    ));
    let before = json_ok(f.run("a", "alice", None, &["--json", "board"]));
    assert_eq!(before["data"]["claims"][id]["session"], bob);
    assert_eq!(before["data"]["reservations"][rv]["session"], bob);
    let token = before["data"]["sessions"][&bob]["token"].as_str().unwrap();
    json_ok(f.run(
        "a",
        "alice",
        Some(&alice),
        &[
            "--json",
            "authority",
            "recover-session",
            &bob,
            "--expect-session",
            token,
            "--reason",
            "holder crashed",
            "--to-session",
            &alice,
        ],
    ));
    let second = f.run(
        "a",
        "alice",
        Some(&alice),
        &[
            "--json",
            "authority",
            "recover-session",
            &bob,
            "--expect-session",
            token,
            "--reason",
            "duplicate recovery",
        ],
    );
    assert_eq!(second.status.code(), Some(2));
    let after = json_ok(f.run("a", "alice", None, &["--json", "authority", "refresh"]));
    assert_eq!(after["data"]["claims"][id]["session"], alice);
    assert_ne!(
        after["data"]["claims"][id]["token"],
        before["data"]["claims"][id]["token"]
    );
    assert_eq!(after["data"]["sessions"][&bob]["status"], "revoked");
    let stale = f.run("b", "bob", Some(&bob), &["--json", "release", id]);
    assert!(!stale.status.success());
    let transport = GitTransport::open(
        &f.repo("a"),
        "origin",
        "mote-state",
        &f.td.path().join("history-intents"),
    )
    .unwrap();
    let client =
        connection::open_client(&f.a, transport, &connection::default_cache_root(&f.a)).unwrap();
    for (request_id, action) in [
        (
            "stale-recovery-history",
            Action::SessionRecover(SessionRecover {
                source_session: bob.clone(),
                expect_session: token.into(),
                claims: std::collections::BTreeMap::from([(
                    id.into(),
                    before["data"]["claims"][id]["token"]
                        .as_str()
                        .unwrap()
                        .into(),
                )]),
                reservations: std::collections::BTreeMap::new(),
                disposition: Disposition::Release,
                recipient_session: None,
                reason: "observed stale recovery".into(),
            }),
        ),
        (
            "stale-reservation-history",
            Action::ReservationRelease {
                reservation: rv.into(),
                token: before["data"]["reservations"][rv]["token"]
                    .as_str()
                    .unwrap()
                    .into(),
            },
        ),
    ] {
        let result = client
            .submit(
                Request {
                    version: 1,
                    request_id: request_id.into(),
                    actor: "alice".into(),
                    session: alice.clone(),
                    timestamp: mote::ids::format_rfc3339(jiff::Timestamp::now()),
                    command: ProtocolCommand::Coordination { action },
                },
                4,
            )
            .unwrap();
        assert_eq!(
            result.status,
            mote::git_authority::client::SubmissionStatus::Rejected
        );
    }
    let history = json_ok(f.run(
        "a",
        "alice",
        None,
        &["--json", "history", id, "--include-rejected"],
    ));
    assert!(history["data"].as_array().unwrap().len() >= 5);
    let accepted = json_ok(f.run("a", "alice", None, &["--json", "history", id]));
    for request_id in ["stale-recovery-history", "stale-reservation-history"] {
        assert!(
            history["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["request_id"] == request_id)
        );
        assert!(
            !accepted["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["request_id"] == request_id)
        );
    }
    json_ok(f.run("a", "alice", Some(&alice), &["--json", "session", "end"]));
    let ready = json_ok(f.run("a", "alice", None, &["--json", "ready"]));
    assert_eq!(ready["data"][0]["id"], id);
    fs::rename(
        f.td.path().join("remote.git"),
        f.td.path().join("offline.git"),
    )
    .unwrap();
    git(&f.repo("a"), &["remote", "remove", "origin"]);
    let cached = json_ok(f.run("a", "alice", None, &["--json", "authority", "cached"]));
    assert_provenance(&cached, false);
    assert!(
        !f.run("a", "alice", None, &["--json", "board"])
            .status
            .success()
    );
    let offline_start = f.run("a", "alice", None, &["--json", "session", "start"]);
    assert!(!offline_start.status.success());
    assert!(!String::from_utf8_lossy(&offline_start.stdout).contains("activate"));
    assert_eq!(fs::read_dir(f.a.ops_dir()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(f.b.ops_dir()).unwrap().count(), 0);
}

#[test]
fn remote_session_as_can_enroll_without_a_prior_local_actor() {
    let f = Fixture::new();
    json_ok(f.run("a", "setup", None, &["--json", "authority", "init-git"]));
    let out = Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(f.repo("a"))
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_SESSION")
        .args(["--json", "session", "start", "--as", "newactor"])
        .output()
        .unwrap();
    let value = json_ok(out);
    assert_eq!(value["actor"], "newactor");
    assert!(value["activate"].as_str().unwrap().contains("MOTE_SESSION"));
}

#[test]
#[cfg(unix)]
fn accepted_session_start_is_shell_evaluable_and_failed_start_exports_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    json_ok(f.run("a", "setup", None, &["--json", "authority", "init-git"]));
    let out = Command::new("/bin/sh")
        .current_dir(f.repo("a"))
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .env_remove("MOTE_STORE")
        .env("MOTE_TEST_BIN", env!("CARGO_BIN_EXE_mote"))
        .env("MOTE_TEST_ACTOR", "shell'actor $literal")
        .args([
            "-c",
            r#"set -eu
activation="$("$MOTE_TEST_BIN" session start --as "$MOTE_TEST_ACTOR")"
eval "$activation"
test "$MOTE_ACTOR" = "$MOTE_TEST_ACTOR"
test -n "$MOTE_SESSION"
printf '%s\n' "$MOTE_SESSION"
"#,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .starts_with("sess-")
    );
    let invalid_label = "x".repeat(4097);
    let rejected = f.run(
        "a",
        "alice",
        None,
        &["session", "start", "--label", &invalid_label],
    );
    assert!(!rejected.status.success());
    assert!(rejected.stdout.is_empty());
    let hooks = f.td.path().join("remote.git/hooks");
    let hook = hooks.join("pre-receive");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &f.td.path().join("remote.git"),
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let uncertain = f.run("a", "alice", None, &["session", "start"]);
    assert_eq!(uncertain.status.code(), Some(4));
    assert!(uncertain.stdout.is_empty());
    assert!(String::from_utf8_lossy(&uncertain.stderr).contains("authority retry"));
}

#[test]
fn explicit_unrelated_store_refuses_before_push_and_linked_worktree_is_supported() {
    let f = Fixture::new();
    let other = f.td.path().join("other.git");
    git(
        f.td.path(),
        &["init", "--bare", "-q", other.to_str().unwrap()],
    );
    git(
        &f.repo("b"),
        &["remote", "set-url", "origin", other.to_str().unwrap()],
    );
    let original = fs::read(f.a.format_path()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(f.repo("b"))
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .arg("--store")
        .arg(f.a.root())
        .args(["--actor", "alice", "--json", "authority", "init-git"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("different Git repository"));
    assert_eq!(fs::read(f.a.format_path()).unwrap(), original);
    for selector in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_NAMESPACE",
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
        command
            .current_dir(f.repo("b"))
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .env_remove("MOTE_SESSION");
        for name in [
            "GIT_DIR",
            "GIT_COMMON_DIR",
            "GIT_WORK_TREE",
            "GIT_NAMESPACE",
        ] {
            command.env_remove(name);
        }
        command.env(
            selector,
            if selector == "GIT_WORK_TREE" {
                f.repo("b")
            } else {
                f.repo("b").join(".git")
            },
        );
        let out = command
            .arg("--store")
            .arg(f.a.root())
            .args(["--actor", "alice", "--json", "authority", "init-git"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{selector}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("unset {selector}")));
        assert!(!connection::setup_path(f.a.root()).exists());
        assert!(!connection::descriptor_path(f.a.root()).exists());
        assert_eq!(fs::read(f.a.format_path()).unwrap(), original);
    }
    for remote in [other, f.td.path().join("remote.git")] {
        let heads = Command::new("git")
            .arg("--git-dir")
            .arg(remote)
            .args(["for-each-ref", "refs/heads"])
            .output()
            .unwrap();
        assert!(heads.status.success());
        assert!(heads.stdout.is_empty());
    }
    fs::write(f.repo("a").join("source.txt"), "source\n").unwrap();
    git(&f.repo("a"), &["config", "user.name", "Fixture"]);
    git(
        &f.repo("a"),
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&f.repo("a"), &["add", "source.txt"]);
    git(&f.repo("a"), &["commit", "-q", "-m", "source"]);
    let worktree = f.td.path().join("linked");
    git(
        &f.repo("a"),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    let out = Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(worktree)
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .arg("--store")
        .arg(f.a.root())
        .args(["--actor", "alice", "--json", "authority", "init-git"])
        .output()
        .unwrap();
    json_ok(out);
    assert_eq!(f.a.read_format().unwrap().schema_version, 2);
}
fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cli_begin_and_done_are_single_remote_lifecycle_transactions() {
    let f = Fixture::new();
    let boot = f.run("a", "alice", None, &["--json", "authority", "init-git"]);
    assert!(
        boot.status.success(),
        "{}",
        String::from_utf8_lossy(&boot.stderr)
    );
    let join = f.run("b", "bob", None, &["--json", "authority", "connect"]);
    assert!(
        join.status.success(),
        "{}",
        String::from_utf8_lossy(&join.stderr)
    );
    let alice = f.start("a", "alice");
    let bob = f.start("b", "bob");
    let created = f.run("a", "alice", Some(&alice), &["--json", "new", "one task"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let id = serde_json::from_slice::<Value>(&created.stdout).unwrap()["receipt"]["outcome"]["result"]["subject"].as_str().unwrap().to_owned();
    let begin = f.run(
        "a",
        "alice",
        Some(&alice),
        &[
            "--json",
            "begin",
            &id,
            "--paths",
            "src/cli.rs",
            "--note",
            "working",
        ],
    );
    assert!(
        begin.status.success(),
        "{}",
        String::from_utf8_lossy(&begin.stderr)
    );
    let begin_json: Value = serde_json::from_slice(&begin.stdout).unwrap();
    assert_eq!(begin_json["receipt"]["outcome"]["result"]["task"], id);
    let conflict = f.run(
        "b",
        "bob",
        Some(&bob),
        &["--json", "begin", &id, "--paths", "src/cli.rs"],
    );
    assert_eq!(
        conflict.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&conflict.stderr)
    );
    let done = f.run(
        "a",
        "alice",
        Some(&alice),
        &["--json", "done", &id, "--note", "complete"],
    );
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stderr)
    );
    let transport = GitTransport::open(
        &f.repo("b"),
        "origin",
        "mote-state",
        &f.td.path().join("inspect"),
    )
    .unwrap();
    let client = connection::open_client(
        f.store("b"),
        transport,
        &connection::default_cache_root(f.store("b")),
    )
    .unwrap();
    let state = client.refresh().unwrap().state.coordination.view().unwrap();
    assert_eq!(state["tasks"][&id]["status"], "closed");
    assert!(state["claims"].get(&id).is_none());
    assert!(
        state["reservations"]
            .as_object()
            .unwrap()
            .values()
            .all(|r| r["status"] == "closed")
    );
    let ttl = f.run("a", "alice", Some(&alice), &["begin", &id, "--ttl", "1s"]);
    assert!(!ttl.status.success());
    assert!(String::from_utf8_lossy(&ttl.stderr).contains("--ttl"));
    let forced = f.run("a", "alice", Some(&alice), &["done", &id, "--force"]);
    assert!(!forced.status.success());
    assert!(String::from_utf8_lossy(&forced.stderr).contains("--force"));
}
