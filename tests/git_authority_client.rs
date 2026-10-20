use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use mote::git_authority::client::{Client, FailureCause, JournalEntry, SubmissionStatus};
use mote::git_authority::protocol::{Manifest, Request, encode};
use mote::git_authority::transport::GitTransport;
use serde_json::json;
use tempfile::{TempDir, tempdir};

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git test setup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    _td: TempDir,
    root: PathBuf,
    remote: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let td = tempdir().unwrap();
        let root = td.path().to_path_buf();
        let remote = root.join("remote.git");
        let a = root.join("a");
        let b = root.join("b");
        for dir in [&remote, &a, &b] {
            fs::create_dir(dir).unwrap();
        }
        git(&remote, &["init", "--bare", "-q"]);
        for repo in [&a, &b] {
            git(repo, &["init", "-q"]);
            git(repo, &["config", "user.name", "Test"]);
            git(repo, &["config", "user.email", "test@example.invalid"]);
            git(repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        }
        let transport =
            GitTransport::open(&a, "origin", "mote-state", &root.join("bootstrap")).unwrap();
        let files = BTreeMap::from([(
            "manifest.json".into(),
            encode(&Manifest::new("st-client-test".into())).unwrap(),
        )]);
        let oid = transport.create_commit(None, &files).unwrap();
        assert!(transport.push(&oid).unwrap().success);
        Self {
            _td: td,
            root,
            remote,
            a,
            b,
        }
    }

    fn client(&self, repo: &Path, name: &str) -> Client {
        let scratch = self.root.join("clients").join(name);
        let transport =
            GitTransport::open(repo, "origin", "mote-state", &scratch.join("git")).unwrap();
        Client::connect(transport, &scratch.join("cache")).unwrap()
    }
}

fn request(id: &str, key: &str, value: &str) -> Request {
    serde_json::from_value(json!({
        "version":1,"request_id":id,"actor":id,"session":format!("sess-{id}"),
        "timestamp":"2026-10-20T00:00:00Z",
        "command":{"kind":"synthetic.cas","changes":[{"key":key,"expected":null,"value":value}]}
    }))
    .unwrap()
}

#[test]
fn two_independent_clients_have_one_winner_and_agree_on_history() {
    let f = Fixture::new();
    let a = f.client(&f.a, "client-a");
    let b = f.client(&f.b, "client-b");
    let barrier = Arc::new(Barrier::new(2));
    let ba = barrier.clone();
    let first = thread::spawn(move || {
        ba.wait();
        a.submit(request("alice", "task", "alice"), 8).unwrap()
    });
    let second = thread::spawn(move || {
        barrier.wait();
        b.submit(request("bob", "task", "bob"), 8).unwrap()
    });
    let outcomes = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(
        outcomes
            .iter()
            .filter(|s| s.status == SubmissionStatus::Accepted)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|s| s.status == SubmissionStatus::Rejected)
            .count(),
        1
    );
    let a = f.client(&f.a, "client-a").refresh().unwrap();
    let b = f.client(&f.b, "client-b").refresh().unwrap();
    assert_eq!(a.head, b.head);
    assert_eq!(a.state, b.state);
    assert_eq!(a.receipts, b.receipts);
    assert_eq!(a.state.sequence, 2);
}

#[test]
fn retained_id_recovers_acceptance_and_rejection_after_cache_loss() {
    let f = Fixture::new();
    let client = f.client(&f.a, "original");
    let r = request("one", "task", "alice");
    let first = client.submit(r.clone(), 3).unwrap();
    assert_eq!(first.status, SubmissionStatus::Accepted);
    let rejected = request("two", "task", "bob");
    assert_eq!(
        client.submit(rejected.clone(), 3).unwrap().status,
        SubmissionStatus::Rejected
    );
    drop(client);
    fs::remove_dir_all(f.root.join("clients/original")).unwrap();
    let fresh = f.client(&f.b, "replacement");
    let recovered = fresh.submit(r.clone(), 3).unwrap();
    assert!(recovered.recovered);
    assert_eq!(recovered.receipt, first.receipt);
    assert_eq!(
        fresh.submit(rejected, 3).unwrap().status,
        SubmissionStatus::Rejected
    );
    let changed = request("one", "another", "alice");
    assert!(fresh.submit(changed, 3).is_err());
    assert_eq!(fresh.refresh().unwrap().state.sequence, 2);
    assert!(fresh.cached_view().unwrap().is_some_and(|v| !v.fresh));
}

#[test]
fn duplicate_requests_from_two_clients_produce_one_receipt() {
    let f = Fixture::new();
    let a = f.client(&f.a, "a");
    let b = f.client(&f.b, "b");
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let one = thread::spawn(move || {
        other.wait();
        a.submit(request("same", "task", "alice"), 8).unwrap()
    });
    let two = thread::spawn(move || {
        barrier.wait();
        b.submit(request("same", "task", "alice"), 8).unwrap()
    });
    let a = one.join().unwrap();
    let b = two.join().unwrap();
    assert_eq!(a.status, SubmissionStatus::Accepted);
    assert_eq!(a.receipt, b.receipt);
    assert_eq!(f.client(&f.a, "read").refresh().unwrap().state.sequence, 1);
}

#[cfg(unix)]
fn hook(remote: &Path, name: &str, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    let path = remote.join("hooks").join(name);
    fs::write(&path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
#[cfg(unix)]
fn denied_push_is_unconfirmed_and_same_request_can_retry_later() {
    let f = Fixture::new();
    let client = f.client(&f.a, "a");
    hook(&f.remote, "pre-receive", "#!/bin/sh\nexit 1\n");
    let r = request("denied", "task", "alice");
    let result = client.submit(r.clone(), 2).unwrap();
    assert_eq!(result.status, SubmissionStatus::Unconfirmed);
    assert!(result.receipt.is_none());
    assert!(result.attempted_commit.is_some());
    assert_eq!(
        result.diagnostic.as_ref().unwrap().cause,
        FailureCause::Denied
    );
    let journal: JournalEntry = serde_json::from_slice(
        &fs::read(f.root.join("clients/a/cache/requests/denied.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        journal.attempted_commits.len(),
        1,
        "denial must not be retried as contention"
    );
    assert_eq!(client.refresh().unwrap().state.sequence, 0);
    fs::remove_file(f.remote.join("hooks/pre-receive")).unwrap();
    assert_eq!(
        client.retry_pending(&r.request_id, 3).unwrap().status,
        SubmissionStatus::Accepted
    );
}

#[test]
#[cfg(unix)]
fn actual_delayed_response_after_server_commit_is_recoverable() {
    let f = Fixture::new();
    // A post-receive delay occurs after the authoritative ref has advanced.
    hook(
        &f.remote,
        "post-receive",
        "#!/bin/sh\n: >mote-test-accepted\nsleep 5\n",
    );
    let scratch = f.root.join("timeout");
    let transport = GitTransport::open(&f.a, "origin", "mote-state", &scratch.join("git"))
        .unwrap()
        .with_timeout(Duration::from_secs(2));
    let client = Client::connect(transport, &scratch.join("cache")).unwrap();
    let r = request("delayed", "task", "alice");
    let result = client.submit(r.clone(), 2).unwrap();
    assert!(matches!(
        result.status,
        SubmissionStatus::Accepted | SubmissionStatus::Unconfirmed
    ));
    assert_eq!(
        result.diagnostic.as_ref().unwrap().cause,
        FailureCause::Timeout
    );
    assert!(
        f.remote.join("mote-test-accepted").exists(),
        "the response delay must occur after server acceptance"
    );
    fs::remove_file(f.remote.join("hooks/post-receive")).unwrap();
    let fresh = f.client(&f.b, "fresh");
    assert!(
        fresh.receipt(&r.request_id).unwrap().is_some(),
        "recover an existing server receipt rather than publishing a replacement"
    );
    let recovered = fresh.submit(r, 3).unwrap();
    assert_eq!(recovered.status, SubmissionStatus::Accepted);
    assert!(recovered.recovered);
    assert_eq!(fresh.refresh().unwrap().state.sequence, 1);
}

#[test]
#[cfg(unix)]
fn rewind_after_an_uncertain_attempt_retains_the_attempt_and_protocol_diagnostic() {
    let f = Fixture::new();
    let client = f.client(&f.a, "a");
    let genesis = client.refresh().unwrap().head;
    assert_eq!(
        client
            .submit(request("first", "one", "alice"), 2)
            .unwrap()
            .status,
        SubmissionStatus::Accepted
    );
    hook(&f.remote, "pre-receive", "#!/bin/sh\nexit 1\n");
    let uncertain = client
        .submit(request("pending", "two", "alice"), 2)
        .unwrap();
    assert_eq!(uncertain.status, SubmissionStatus::Unconfirmed);
    git(
        &f.remote,
        &["update-ref", "refs/heads/mote-state", &genesis],
    );
    let retry = client.retry_pending("pending", 2).unwrap();
    assert_eq!(retry.status, SubmissionStatus::Unconfirmed);
    assert_eq!(retry.attempted_commit, uncertain.attempted_commit);
    assert_eq!(retry.diagnostic.unwrap().cause, FailureCause::Incompatible);
    assert!(client.refresh().is_err());
}

#[test]
fn missing_bound_branch_refuses_fallback_and_reinitialization() {
    let f = Fixture::new();
    let client = f.client(&f.a, "a");
    git(&f.remote, &["update-ref", "-d", "refs/heads/mote-state"]);
    assert!(client.refresh().is_err());
    assert!(
        client
            .submit(request("missing", "task", "alice"), 2)
            .is_err()
    );
    assert!(
        Client::connect(
            GitTransport::open(&f.b, "origin", "mote-state", &f.root.join("new-git")).unwrap(),
            &f.root.join("new-cache")
        )
        .is_err()
    );
}
