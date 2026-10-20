use mote::Store;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    root: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let f = Self {
            root: tempdir().unwrap(),
        };
        let remote = f.root.path().join("remote.git");
        git(
            f.root.path(),
            &["init", "--bare", "-q", remote.to_str().unwrap()],
        );
        for name in ["a", "b", "c"] {
            let repo = f.root.path().join(name);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
            Store::init(&repo).unwrap();
        }
        f.ok("a", "alice", None, &["authority", "init-git"]);
        for name in ["b", "c"] {
            f.ok(name, "bob", None, &["authority", "connect"]);
        }
        f
    }
    fn command(&self, name: &str, actor: &str, session: Option<&str>, args: &[&str]) -> Command {
        let repo = self.root.path().join(name);
        let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
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
        decode(self.run(name, actor, session, args), 0)
    }
    fn start(&self, name: &str, actor: &str) -> String {
        self.ok(name, actor, None, &["session", "start", "--as", actor])["session_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn sequence(&self) -> u64 {
        self.ok("a", "alice", None, &["authority", "refresh"])["authority"]["sequence"]
            .as_u64()
            .unwrap()
    }
    fn no_local_ops(&self) {
        for name in ["a", "b", "c"] {
            assert_eq!(
                fs::read_dir(self.root.path().join(name).join(".mote/ops"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}
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
fn git(repo: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    isolate(&mut command);
    let result = command.arg("-C").arg(repo).args(args).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn decode(output: Output, expected: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn subject(value: &Value) -> &str {
    value["receipt"]["outcome"]["result"]["subject"]
        .as_str()
        .unwrap()
}
fn provenance(value: &Value) {
    assert_eq!(value["authority"]["fresh"], true);
    for key in ["head", "root", "store_id", "observed_at"] {
        assert!(value["authority"][key].is_string(), "{key}");
    }
    assert!(value["authority"]["sequence"].is_number());
}

#[test]
fn direct_messages_have_canonical_races_receipt_retries_and_passive_reads() {
    let f = Fixture::new();
    let alice = f.start("a", "alice");
    let bob = f.start("b", "bob");
    let bob_other = f.start("c", "bob");
    let sent = f.ok(
        "a",
        "alice",
        Some(&alice),
        &[
            "send",
            "bob",
            "--kind",
            "request",
            "--idempotency-key",
            "fixture-request",
            "please review",
        ],
    );
    let id = subject(&sent).to_owned();
    let before = f.sequence();
    let inbox = f.ok("b", "bob", None, &["inbox"]);
    provenance(&inbox);
    assert_eq!(inbox["data"][0]["msg_id"], id);
    let thread = f.ok("a", "alice", None, &["msg", "thread", "bob"]);
    provenance(&thread);
    assert_eq!(thread["data"][0]["body"], "please review");
    assert_eq!(f.sequence(), before);
    let mut a = f
        .command("b", "bob", Some(&bob), &["msg", "ack", &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let b = f
        .command("c", "bob", Some(&bob_other), &["msg", "ack", &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let b = match b {
        Ok(b) => b,
        Err(error) => {
            let _ = a.kill();
            let _ = a.wait();
            panic!("second child spawn: {error}")
        }
    };
    let first = a.wait_with_output().unwrap();
    let second = b.wait_with_output().unwrap();
    let mut codes = [first.status.code().unwrap(), second.status.code().unwrap()];
    codes.sort();
    assert_eq!(codes, [0, 2]);
    let reply = f.ok("b", "bob", Some(&bob), &["msg", "reply", &id, "reviewed"]);
    let repeated = f.ok(
        "b",
        "bob",
        None,
        &["authority", "retry", reply["request_id"].as_str().unwrap()],
    );
    assert_eq!(repeated["receipt"], reply["receipt"]);
    let rejected = decode(
        f.run(
            "c",
            "bob",
            Some(&bob_other),
            &["msg", "reply", &id, "another response"],
        ),
        2,
    );
    assert_eq!(
        rejected["receipt"]["outcome"]["result"]["code"],
        "communication_effect_rejected"
    );
    f.ok("a", "alice", Some(&alice), &["msg", "resolve", &id]);
    let requests = f.ok(
        "a",
        "alice",
        None,
        &["msg", "requests", "--state", "resolved"],
    );
    assert_eq!(requests["data"][0]["msg_id"], id);
    let duplicate = decode(
        f.run(
            "a",
            "alice",
            Some(&alice),
            &[
                "msg",
                "send",
                "--to",
                "bob",
                "--kind",
                "request",
                "--idempotency-key",
                "fixture-request",
                "please review",
            ],
        ),
        2,
    );
    assert_eq!(
        duplicate["receipt"]["outcome"]["result"]["code"],
        "communication_effect_rejected"
    );
    let before = f.sequence();
    let unsupported = f.run(
        "a",
        "alice",
        Some(&alice),
        &[
            "msg",
            "send",
            "--to",
            "bob",
            "--require-live",
            "cannot promise",
        ],
    );
    assert!(!unsupported.status.success());
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("--require-live"));
    assert_eq!(f.sequence(), before);
    f.no_local_ops();
}

#[test]
fn discussions_watch_read_promote_and_route_with_complete_provenance() {
    let f = Fixture::new();
    let alice = f.start("a", "alice");
    let bob = f.start("b", "bob");
    f.ok(
        "a",
        "alice",
        Some(&alice),
        &["discuss", "topic", "new", "design"],
    );
    f.ok("b", "bob", Some(&bob), &["discuss", "watch", "design"]);
    let posted = f.ok(
        "a",
        "alice",
        Some(&alice),
        &["discuss", "post", "--topic", "design", "a useful design"],
    );
    let post = subject(&posted).to_owned();
    let notifications = f.ok(
        "b",
        "bob",
        None,
        &["discuss", "notifications", "--topic", "design"],
    );
    provenance(&notifications);
    assert_eq!(notifications["data"]["posts"][0]["post_id"], post);
    f.ok(
        "b",
        "bob",
        Some(&bob),
        &[
            "discuss",
            "mark-read",
            "--topic",
            "design",
            "--through",
            &post,
        ],
    );
    assert!(
        f.ok(
            "b",
            "bob",
            None,
            &["discuss", "unread", "--topic", "design"]
        )["data"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let promoted = f.ok(
        "b",
        "bob",
        Some(&bob),
        &[
            "discuss",
            "promote",
            &post,
            "--tag",
            "design",
            "--title",
            "implement design",
        ],
    );
    let task = subject(&promoted).to_owned();
    let effects = promoted["receipt"]["outcome"]["result"]["effect_ids"]
        .as_array()
        .unwrap();
    assert_eq!(effects.len(), 4); // create, tag, route, provenance note
    let shown = f.ok("a", "alice", None, &["show", &task]);
    assert_eq!(shown["data"]["title"], "implement design");
    assert!(shown["data"]["body"].as_str().unwrap().contains(&post));
    assert_eq!(shown["data"]["notes"].as_array().unwrap().len(), 1);
    let after = f.sequence();
    let repeated = f.ok(
        "b",
        "bob",
        None,
        &[
            "authority",
            "retry",
            promoted["request_id"].as_str().unwrap(),
        ],
    );
    assert_eq!(repeated["receipt"], promoted["receipt"]);
    assert_eq!(f.sequence(), after);
    f.ok(
        "a",
        "alice",
        Some(&alice),
        &[
            "discuss",
            "route",
            &post,
            "--issue",
            &task,
            "--note",
            "agreed implementation",
        ],
    );
    let shown = f.ok("b", "bob", None, &["show", &task]);
    assert_eq!(shown["data"]["notes"].as_array().unwrap().len(), 2);
    let before = f.sequence();
    for args in [
        vec!["discuss", "list", "--topic", "design"],
        vec!["discuss", "topics"],
        vec!["discuss", "thread", &post],
        vec!["discuss", "search", "useful"],
        vec!["discuss", "pulse"],
    ] {
        provenance(&f.ok("a", "alice", None, &args));
    }
    assert_eq!(f.sequence(), before);
    f.no_local_ops();
}
