use mote::{
    Store,
    git_authority::{
        client::SubmissionStatus,
        connection,
        coordination::{Action, communications},
        protocol::{Command, Request, Transaction},
        transport::GitTransport,
    },
};
use serde_json::json;
use std::{fs, path::Path, process::Command as ProcessCommand};
use tempfile::tempdir;

fn git(repo: &Path, args: &[&str]) {
    let result = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn request(id: &str, session: &str, action: Action) -> Request {
    Request {
        version: 1,
        request_id: id.into(),
        actor: "bob".into(),
        session: session.into(),
        timestamp: "2026-10-20T00:00:00Z".into(),
        command: Command::Coordination { action },
    }
}

#[test]
fn two_independent_clients_reject_a_stale_same_actor_cursor_and_recover_its_receipt() {
    let root = tempdir().unwrap();
    let remote = root.path().join("remote.git");
    git(
        root.path(),
        &["init", "--bare", "-q", remote.to_str().unwrap()],
    );
    let mut stores = Vec::new();
    for name in ["a", "b"] {
        let path = root.path().join(name);
        fs::create_dir(&path).unwrap();
        git(&path, &["init", "-q"]);
        git(
            &path,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        stores.push(Store::init(&path).unwrap());
    }
    let transport = |name: &str, label: &str| {
        GitTransport::open(
            &root.path().join(name),
            "origin",
            "mote-state",
            &root.path().join(label),
        )
        .unwrap()
    };
    let cache_a = connection::default_cache_root(&stores[0]);
    let cache_b = connection::default_cache_root(&stores[1]);
    connection::bootstrap(&stores[0], transport("a", "bootstrap"), &cache_a).unwrap();
    connection::connect(&stores[1], transport("b", "connect"), &cache_b).unwrap();
    let a = connection::open_client(&stores[0], transport("a", "client-a"), &cache_a).unwrap();
    let b = connection::open_client(&stores[1], transport("b", "client-b"), &cache_b).unwrap();
    for (client, id, session) in [(&a, "start-a", "session-a"), (&b, "start-b", "session-b")] {
        assert_eq!(
            client
                .submit(
                    request(id, session, Action::SessionStart { label: id.into() }),
                    4
                )
                .unwrap()
                .status,
            SubmissionStatus::Accepted
        );
    }
    let posted = a
        .submit(
            request(
                "post",
                "session-a",
                Action::Communication {
                    action: communications::Action::Post {
                        id: "post-one".into(),
                        topic: "design".into(),
                        body: "shared post".into(),
                        reply_to: None,
                        post_kind: "post".into(),
                        answers: vec![],
                        notify: vec![],
                        idempotency_key: None,
                    },
                },
            ),
            4,
        )
        .unwrap();
    assert_eq!(posted.status, SubmissionStatus::Accepted);
    let first = a.refresh().unwrap();
    let second = b.refresh().unwrap();
    assert_eq!(first.head, second.head);
    for snapshot in [&first, &second] {
        assert!(
            snapshot
                .state
                .coordination
                .communication_state()
                .unwrap()
                .discussion_cursor_for("bob", Some("design"))
                .is_none()
        );
    }
    let read = |id, session| {
        request(
            id,
            session,
            Action::Communication {
                action: communications::Action::Read {
                    topic: Some("design".into()),
                    through_post: "post-one".into(),
                    expect: None,
                },
            },
        )
    };
    assert_eq!(
        a.submit(read("read-a", "session-a"), 4).unwrap().status,
        SubmissionStatus::Accepted
    );
    let stale_request = read("read-b", "session-b");
    let stale = b.submit(stale_request.clone(), 4).unwrap();
    assert_eq!(stale.status, SubmissionStatus::Rejected);
    let receipt = stale.receipt.unwrap();
    assert_eq!(
        receipt.outcome.result,
        json!({"code":"stale_cursor","subject":"post-one"})
    );
    let snapshot = b.refresh().unwrap();
    let transaction: Transaction = serde_json::from_slice(
        &snapshot.files[&format!("transactions/{:020}.json", receipt.sequence)],
    )
    .unwrap();
    assert!(transaction.effects.is_empty());
    let replacement =
        connection::open_client(&stores[1], transport("b", "replacement"), &cache_b).unwrap();
    let recovered = replacement.submit(stale_request, 4).unwrap();
    assert_eq!(recovered.receipt.unwrap(), receipt);
    assert_eq!(replacement.refresh().unwrap().head, snapshot.head);
    assert_eq!(a.refresh().unwrap().state, snapshot.state);
    for store in stores {
        assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 0);
    }
}
