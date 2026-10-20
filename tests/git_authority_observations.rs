use mote::{Store, git_authority::connection};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::mpsc,
    time::Duration,
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
        git(
            f.root.path(),
            &[
                "init",
                "--bare",
                "-q",
                f.root.path().join("remote.git").to_str().unwrap(),
            ],
        );
        for name in ["a", "b"] {
            let path = f.repo(name);
            fs::create_dir(&path).unwrap();
            git(&path, &["init", "-q"]);
            git(
                &path,
                &[
                    "remote",
                    "add",
                    "origin",
                    f.root.path().join("remote.git").to_str().unwrap(),
                ],
            );
            Store::init(&path).unwrap();
        }
        f.ok("a", None, &["authority", "init-git"]);
        f.ok("b", None, &["authority", "connect"]);
        f
    }
    fn repo(&self, name: &str) -> std::path::PathBuf {
        self.root.path().join(name)
    }
    fn command(&self, name: &str, session: Option<&str>, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_mote"));
        isolate(&mut c);
        c.current_dir(self.repo(name))
            .args([
                "--actor",
                if name == "a" { "alice" } else { "bob" },
                "--json",
            ])
            .arg("--store")
            .arg(self.repo(name).join(".mote"))
            .args(args);
        if let Some(session) = session {
            c.env("MOTE_SESSION", session);
        }
        c
    }
    fn run(&self, name: &str, session: Option<&str>, args: &[&str]) -> Output {
        self.command(name, session, args).output().unwrap()
    }
    fn ok(&self, name: &str, session: Option<&str>, args: &[&str]) -> Value {
        let result = self.run(name, session, args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
    fn start(&self, name: &str) -> String {
        self.ok(name, None, &["session", "start"])["session_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn events(&self, name: &str, args: &[&str]) -> Vec<Value> {
        let output = self.run(name, None, args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn sequence(&self) -> u64 {
        self.ok("a", None, &["authority", "refresh"])["authority"]["sequence"]
            .as_u64()
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
fn isolate(c: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") || key.to_string_lossy().starts_with("MOTE_") {
            c.env_remove(key);
        }
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
}
fn git(path: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    isolate(&mut c);
    let output = c.arg("-C").arg(path).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn fresh(value: &Value) {
    assert_eq!(value["authority"]["fresh"], true);
    assert_eq!(value["authority"]["ownership"], "explicit-release-v1");
    assert!(value["authority"]["root"].is_string());
}

#[test]
fn actors_and_diagnostics_use_explicit_remote_ownership_without_read_writes() {
    let f = Fixture::new();
    let session = f.start("a");
    f.start("b");
    let created = f.ok("a", Some(&session), &["new", "shared task"]);
    let id = created["receipt"]["outcome"]["result"]["subject"]
        .as_str()
        .unwrap();
    f.ok(
        "a",
        Some(&session),
        &["begin", id, "--paths", "src/demo.rs"],
    );
    f.ok("a", Some(&session), &["send", "bob", "please inspect"]);
    let before = f.sequence();
    for observer in ["a", "b"] {
        let status = f.ok(observer, None, &["actor", "status", "alice"]);
        fresh(&status);
        assert_eq!(status["data"]["enrollment"], "active");
        assert_eq!(status["data"]["process_liveness"], "unknown");
        assert_eq!(status["data"]["claims"][0]["task"], id);
        assert_eq!(
            status["data"]["reservations"][0]["reservation"]["paths"][0],
            "src/demo.rs"
        );
        let bob = f.ok(observer, None, &["actor", "status", "bob"]);
        assert_eq!(
            bob["data"]["attention"]["unacked_messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            f.ok(observer, None, &["actor", "list"])["data"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        for command in ["doctor", "fsck", "audit"] {
            let report = f.ok(observer, None, &[command]);
            fresh(&report);
            assert_eq!(report["data"]["scope"], "git-authority-canonical-chain-v1");
            assert_eq!(report["data"]["canonical_chain_verified"], true);
            assert_eq!(
                report["data"]["compatibility"]["structural_supported"],
                true
            );
            assert_eq!(
                report["data"]["binary"]["build"]["status"],
                "unavailable_f10"
            );
        }
    }
    f.ok("a", Some(&session), &["session", "end", &session]);
    let status = f.ok("b", None, &["actor", "status", "alice"]);
    assert_eq!(status["data"]["enrollment"], "inactive");
    assert!(status["data"]["claims"].as_array().unwrap().is_empty());
    assert_eq!(f.sequence(), before + 1);
    f.no_local_ops();
}

#[test]
fn event_cursors_resume_across_clients_and_cache_loss_without_ordering_by_ids() {
    let f = Fixture::new();
    let alice = f.start("a");
    let bob = f.start("b");
    f.ok("a", Some(&alice), &["new", "first"]);
    let before = f.sequence();
    let initial = f.events("a", &["events"]);
    assert_eq!(initial.len(), 3);
    for (index, event) in initial.iter().enumerate() {
        fresh(event);
        assert_eq!(event["data"]["position"]["sequence"], index + 1);
        assert_eq!(event["data"]["phase"], "remote");
    }
    let cursor = initial.last().unwrap()["data"]["cursor"].as_str().unwrap();
    assert!(f.events("b", &["events", "--after", cursor]).is_empty());
    let sent = f.ok("b", Some(&bob), &["send", "alice", "tail message"]);
    let expected_id = sent["receipt"]["outcome"]["result"]["subject"]
        .as_str()
        .unwrap();
    let store = Store::open(&f.repo("a").join(".mote")).unwrap();
    fs::remove_dir_all(connection::default_cache_root(&store)).unwrap();
    // Routing/binding custody is kept with this machine's cache. Restore it
    // explicitly against the immutable store descriptor after total loss.
    f.ok("a", None, &["authority", "connect"]);
    let resumed = f.events(
        "a",
        &[
            "events",
            "--after",
            cursor,
            "--kind",
            "message",
            "--for-actor",
            "alice",
        ],
    );
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0]["data"]["event"]["data"]["msg_id"], expected_id);
    assert_eq!(resumed[0]["data"]["position"]["sequence"], before + 1);
    let other = f.events(
        "b",
        &[
            "events",
            "--after",
            cursor,
            "--kind",
            "message",
            "--for-actor",
            "alice",
        ],
    );
    assert_eq!(other[0]["data"], resumed[0]["data"]);
    assert!(
        f.events(
            "a",
            &["events", "--kind", "message", "--for-actor", "nobody"]
        )
        .is_empty()
    );
    let invalid = f.run(
        "a",
        None,
        &[
            "events",
            "--after",
            "ga:wrong:remote:00000000000000000001:000000",
        ],
    );
    assert_eq!(invalid.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("cursor is absent"));
    let future = format!(
        "ga:{}:remote:99999999999999999999:000000",
        initial[0]["authority"]["root"].as_str().unwrap()
    );
    assert_eq!(
        f.run("a", None, &["events", "--after", &future])
            .status
            .code(),
        Some(3)
    );
    assert_eq!(f.sequence(), before + 1);
    f.no_local_ops();
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[test]
fn watch_and_event_follow_are_read_only_and_label_each_fresh_observation() {
    let f = Fixture::new();
    f.start("a");
    let before = f.sequence();
    for args in [
        &["watch", "--interval", "1"][..],
        &["events", "--follow", "--interval", "1"][..],
    ] {
        let mut child = OwnedChild(
            f.command("b", None, args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = child.0.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = sender.send(result);
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(20))
            .expect("initial read-only projection")
            .unwrap();
        fresh(&serde_json::from_str::<Value>(&line).unwrap());
        drop(child);
        reader.join().unwrap();
    }
    assert_eq!(f.sequence(), before);
    f.no_local_ops();
}
