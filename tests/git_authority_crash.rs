//! Actual-process interruption qualification for the Git authority client.
//!
//! The worker is this integration-test binary re-executed with a private
//! environment.  Shell barriers live only in a `TempDir`; the parent kills the
//! exact worker PID after observing each durable boundary.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use mote::git_authority::{
    client::{Client, SubmissionStatus},
    protocol::{Manifest, Request, encode},
    transport::GitTransport,
};
use serde_json::json;
use tempfile::TempDir;

const WORKER: &str = "MOTE_GA_CRASH_WORKER";
const RACE_WORKER: &str = "MOTE_GA_RACE_WORKER";

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn request() -> Request {
    serde_json::from_value(json!({
        "version":1,"request_id":"crash-request","actor":"crash","session":"crash-session",
        "timestamp":"2026-10-20T00:00:00Z",
        "command":{"kind":"synthetic.cas","changes":[
            {"key":"first","expected":null,"value":"one"},
            {"key":"second","expected":null,"value":"two"}
        ]}
    }))
    .unwrap()
}

fn race_request(id: &str, key: &str, value: &str) -> Request {
    serde_json::from_value(json!({
        "version":1,"request_id":id,"actor":id,"session":format!("session-{id}"),
        "timestamp":"2026-10-20T00:00:00Z",
        "command":{"kind":"synthetic.cas","changes":[{"key":key,"expected":null,"value":value}]}
    }))
    .unwrap()
}

struct Fixture {
    _td: TempDir,
    root: PathBuf,
    remote: PathBuf,
    repo: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let remote = root.join("remote.git");
        let repo = root.join("source");
        fs::create_dir(&remote).unwrap();
        fs::create_dir(&repo).unwrap();
        git(&remote, &["init", "--bare", "-q"]);
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Crash test"]);
        git(&repo, &["config", "user.email", "crash@example.invalid"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        let transport =
            GitTransport::open(&repo, "origin", "mote-state", &root.join("bootstrap")).unwrap();
        let root_commit = transport
            .create_commit(
                None,
                &BTreeMap::from([(
                    "manifest.json".into(),
                    encode(&Manifest::new("st-crash-test".into())).unwrap(),
                )]),
            )
            .unwrap();
        assert!(transport.push(&root_commit).unwrap().success);
        Self {
            _td: td,
            root: root.clone(),
            remote,
            repo,
            cache: root.join("cache"),
        }
    }

    fn client(&self, cache: &Path, scratch_name: &str) -> Client {
        let transport = GitTransport::open(
            &self.repo,
            "origin",
            "mote-state",
            &self.root.join(scratch_name),
        )
        .unwrap();
        Client::connect(transport, cache).unwrap()
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "timed out waiting for {}", path.display());
}

fn worker(f: &Fixture, phase: &str, barrier: &Path, release: &Path, hook_pid: &Path) -> Child {
    let wrapper = f.root.join("wrapper");
    fs::create_dir(&wrapper).unwrap();
    let script = wrapper.join("git");
    fs::write(&script, "#!/bin/sh\n\
if [ \"$MOTE_GA_CRASH_PHASE\" = journal ] && [ -f \"$MOTE_GA_CRASH_CACHE/requests/crash-request.json\" ]; then\n\
  : > \"$MOTE_GA_CRASH_BARRIER\"; while [ ! -f \"$MOTE_GA_CRASH_RELEASE\" ]; do sleep 1; done\n\
fi\n\
is_push=; for arg do [ \"$arg\" != push ] || is_push=1; done\n\
if [ \"$MOTE_GA_CRASH_PHASE\" = attempt ] && [ \"$is_push\" = 1 ] && grep -q 'attempted_commits.*[0-9a-f]' \"$MOTE_GA_CRASH_CACHE/requests/crash-request.json\"; then\n\
  : > \"$MOTE_GA_CRASH_BARRIER\"; while [ ! -f \"$MOTE_GA_CRASH_RELEASE\" ]; do sleep 1; done\n\
fi\n\
exec /usr/bin/git \"$@\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    if phase == "accepted" {
        let hook = f.remote.join("hooks/post-receive");
        fs::write(&hook, "#!/bin/sh\nprintf '%s' \"$$\" > \"$MOTE_GA_HOOK_PID\"\n: > \"$MOTE_GA_CRASH_BARRIER\"\nwhile [ ! -f \"$MOTE_GA_CRASH_RELEASE\" ]; do sleep 1; done\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let path = format!("{}:{}", wrapper.display(), std::env::var("PATH").unwrap());
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_worker", "--nocapture"])
        .env(WORKER, "1")
        .env("MOTE_GA_CRASH_PHASE", phase)
        .env("MOTE_GA_CRASH_REPO", &f.repo)
        .env("MOTE_GA_CRASH_CACHE", &f.cache)
        .env("MOTE_GA_CRASH_SCRATCH", f.root.join("worker-scratch"))
        .env("MOTE_GA_CRASH_BARRIER", barrier)
        .env("MOTE_GA_CRASH_RELEASE", release)
        .env("MOTE_GA_HOOK_PID", hook_pid)
        .env("PATH", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn crash_worker() {
    if std::env::var_os(WORKER).is_none() {
        return;
    }
    let repo = PathBuf::from(std::env::var("MOTE_GA_CRASH_REPO").unwrap());
    let cache = PathBuf::from(std::env::var("MOTE_GA_CRASH_CACHE").unwrap());
    let scratch = PathBuf::from(std::env::var("MOTE_GA_CRASH_SCRATCH").unwrap());
    let transport = GitTransport::open(&repo, "origin", "mote-state", &scratch).unwrap();
    let client = Client::connect(transport, &cache).unwrap();
    let _ = client.submit(request(), 2);
}

#[test]
fn race_worker() {
    if std::env::var_os(RACE_WORKER).is_none() {
        return;
    }
    let repo = PathBuf::from(std::env::var("MOTE_GA_RACE_REPO").unwrap());
    let cache = PathBuf::from(std::env::var("MOTE_GA_RACE_CACHE").unwrap());
    let scratch = PathBuf::from(std::env::var("MOTE_GA_RACE_SCRATCH").unwrap());
    let transport = GitTransport::open(&repo, "origin", "mote-state", &scratch).unwrap();
    let client = Client::connect(transport, &cache).unwrap();
    let result = client
        .submit(
            race_request(
                &std::env::var("MOTE_GA_RACE_ID").unwrap(),
                &std::env::var("MOTE_GA_RACE_KEY").unwrap(),
                &std::env::var("MOTE_GA_RACE_VALUE").unwrap(),
            ),
            8,
        )
        .unwrap();
    fs::write(
        std::env::var("MOTE_GA_RACE_RESULT").unwrap(),
        format!("{:?}", result.status),
    )
    .unwrap();
}

#[cfg(unix)]
fn race_child(
    f: &Fixture,
    label: &str,
    id: &str,
    key: &str,
    value: &str,
    ready: &Path,
    release: &Path,
) -> Child {
    let wrapper = f.root.join(format!("race-wrapper-{label}"));
    fs::create_dir(&wrapper).unwrap();
    let script = wrapper.join("git");
    fs::write(&script, "#!/bin/sh\nis_push=; for arg do [ \"$arg\" != push ] || is_push=1; done\nif [ \"$is_push\" = 1 ]; then : > \"$MOTE_GA_RACE_READY\"; while [ ! -f \"$MOTE_GA_RACE_RELEASE\" ]; do sleep 1; done; fi\nexec /usr/bin/git \"$@\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "race_worker", "--nocapture"])
        .env(RACE_WORKER, "1")
        .env("MOTE_GA_RACE_REPO", &f.repo)
        .env(
            "MOTE_GA_RACE_CACHE",
            f.root.join(format!("race-cache-{label}")),
        )
        .env(
            "MOTE_GA_RACE_SCRATCH",
            f.root.join(format!("race-scratch-{label}")),
        )
        .env("MOTE_GA_RACE_ID", id)
        .env("MOTE_GA_RACE_KEY", key)
        .env("MOTE_GA_RACE_VALUE", value)
        .env("MOTE_GA_RACE_READY", ready)
        .env("MOTE_GA_RACE_RELEASE", release)
        .env(
            "MOTE_GA_RACE_RESULT",
            f.root.join(format!("race-result-{label}")),
        )
        .env(
            "PATH",
            format!("{}:{}", wrapper.display(), std::env::var("PATH").unwrap()),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[cfg(unix)]
fn race_case(
    first: (&str, &str, &str),
    second: (&str, &str, &str),
    expected: (&str, &str),
    values: usize,
    receipts: usize,
) {
    let f = Fixture::new();
    fs::write(f.repo.join("source.txt"), b"tracked\n").unwrap();
    git(&f.repo, &["add", "source.txt"]);
    git(&f.repo, &["commit", "-m", "source"]);
    git(&f.repo, &["push", "origin", "HEAD:refs/heads/source"]);
    git(&f.repo, &["fetch", "origin", "refs/heads/source"]);
    fs::write(f.repo.join("source.txt"), b"dirty\n").unwrap();
    let source = (
        git_text(&f.repo, &["show-ref"]),
        fs::read(f.repo.join(".git/index")).unwrap(),
        fs::read(f.repo.join(".git/FETCH_HEAD")).unwrap(),
        git_text(&f.repo, &["status", "--porcelain"]),
    );
    let release = f.root.join("race-release");
    let ready_a = f.root.join("race-ready-a");
    let ready_b = f.root.join("race-ready-b");
    let mut a = race_child(&f, "a", first.0, first.1, first.2, &ready_a, &release);
    let mut b = race_child(&f, "b", second.0, second.1, second.2, &ready_b, &release);
    wait_for(&ready_a);
    wait_for(&ready_b);
    fs::write(&release, b"go").unwrap();
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    let outcomes = [
        fs::read_to_string(f.root.join("race-result-a")).unwrap(),
        fs::read_to_string(f.root.join("race-result-b")).unwrap(),
    ];
    if expected.0 == expected.1 {
        assert!(outcomes.iter().all(|outcome| outcome == expected.0));
    } else {
        assert!(outcomes.iter().any(|outcome| outcome == expected.0));
        assert!(outcomes.iter().any(|outcome| outcome == expected.1));
    }
    let snapshot = f
        .client(&f.root.join("race-observer-cache"), "race-observer-scratch")
        .refresh()
        .unwrap();
    assert_eq!(snapshot.receipts.len(), receipts);
    assert_eq!(snapshot.state.values.len(), values);
    assert_eq!(
        (
            git_text(&f.repo, &["show-ref"]),
            fs::read(f.repo.join(".git/index")).unwrap(),
            fs::read(f.repo.join(".git/FETCH_HEAD")).unwrap(),
            git_text(&f.repo, &["status", "--porcelain"])
        ),
        source
    );
}

fn git_text(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
#[cfg(unix)]
fn independent_process_races_preserve_intent_and_source_checkout() {
    race_case(
        ("conflict-a", "same", "a"),
        ("conflict-b", "same", "b"),
        ("Accepted", "Rejected"),
        1,
        2,
    );
    race_case(
        ("left", "left", "a"),
        ("right", "right", "b"),
        ("Accepted", "Accepted"),
        2,
        2,
    );
    race_case(
        ("equal", "same", "a"),
        ("equal", "same", "a"),
        ("Accepted", "Accepted"),
        1,
        1,
    );
}

fn assert_no_effects(client: &Client) {
    let snapshot = client.refresh().unwrap();
    assert_eq!(snapshot.state.sequence, 0);
    assert!(snapshot.state.values.is_empty());
    assert!(snapshot.receipts.is_empty());
}

#[cfg(unix)]
fn wait_gone(pid: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let alive = Command::new("kill")
            .args(["-0", "--", pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !alive {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("owned hook process {pid} remained alive");
}

#[test]
#[cfg(unix)]
fn interrupted_children_recover_one_atomic_receipt_at_each_durable_boundary() {
    for phase in ["journal", "attempt", "accepted"] {
        let f = Fixture::new();
        let barrier = f.root.join("barrier");
        let release = f.root.join("release");
        let hook_pid = f.root.join("hook.pid");
        let mut child = worker(&f, phase, &barrier, &release, &hook_pid);
        wait_for(&barrier);
        let journal = fs::read_to_string(f.cache.join("requests/crash-request.json")).unwrap();
        assert!(journal.contains("crash-request"));
        if phase == "attempt" {
            assert!(
                journal.contains("attempted_commits")
                    && journal.matches(|c: char| c.is_ascii_hexdigit()).count() >= 40
            );
        }
        if phase != "accepted" {
            assert_no_effects(&f.client(&f.root.join("observer-cache"), "observer"));
        }
        child.kill().unwrap();
        child.wait().unwrap();
        fs::write(&release, b"release").unwrap();
        if phase == "accepted" {
            wait_for(&hook_pid);
            wait_gone(fs::read_to_string(&hook_pid).unwrap().trim());
        }
        let client = f.client(&f.cache, "recovery");
        let outcome = client.retry_pending("crash-request", 3).unwrap();
        assert_eq!(outcome.status, SubmissionStatus::Accepted, "phase {phase}");
        if phase == "accepted" {
            assert!(outcome.recovered);
        }
        let snapshot = client.refresh().unwrap();
        assert_eq!(snapshot.receipts.len(), 1);
        assert_eq!(snapshot.state.sequence, 1);
        assert_eq!(snapshot.state.values.len(), 2);
        assert_eq!(snapshot.state.values["first"].value, "one");
        assert_eq!(snapshot.state.values["second"].value, "two");
    }
}
