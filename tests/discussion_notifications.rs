//! Explicit public-board attention routing without channel membership or privacy.

use std::fs;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init(temp: &TempDir) {
    let output = Command::new(mote_bin())
        .arg("init")
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(output.status.success());
}

fn output(temp: &TempDir, actor: &str, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(temp.path())
        .output()
        .unwrap()
}

fn success(temp: &TempDir, actor: &str, args: &[&str]) -> Output {
    let output = output(temp, actor, args);
    assert!(
        output.status.success(),
        "mote {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json(temp: &TempDir, actor: &str, args: &[&str]) -> Value {
    serde_json::from_slice(&success(temp, actor, args).stdout).unwrap()
}

fn op_count(temp: &TempDir) -> usize {
    fs::read_dir(temp.path().join(".mote/ops"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

#[test]
fn watches_and_explicit_recipients_create_durable_cursorable_public_attention() {
    let temp = TempDir::new().unwrap();
    init(&temp);
    success(
        &temp,
        "alice",
        &["discuss", "topic", "new", "planning", "--title", "Planning"],
    );
    let watched = json(&temp, "bob", &["--json", "discuss", "watch", "planning"]);
    assert_eq!(watched["topic"], "planning");
    assert_eq!(watched["watching"], true);
    assert_eq!(
        json(&temp, "bob", &["--json", "discuss", "watches"]),
        serde_json::json!(["planning"])
    );

    let first_args = [
        "--json",
        "discuss",
        "post",
        "--topic",
        "planning",
        "--notify",
        "charlie",
        "--notify",
        "alice",
        "--idempotency-key",
        "planning-post-1",
        "public update; @dave is prose only",
    ];
    let first = json(&temp, "alice", &first_args);
    assert_eq!(first["public"], true);
    assert_eq!(first["idempotent_retry"], false);
    assert_eq!(first["explicit_notify"], serde_json::json!(["charlie"]));
    assert_eq!(
        first["notification_recipients"],
        serde_json::json!(["bob", "charlie"])
    );
    let first_post = first["post_id"].as_str().unwrap().to_string();
    let after_first = op_count(&temp);

    let retry = json(&temp, "alice", &first_args);
    assert_eq!(retry["post_id"], first_post);
    assert_eq!(retry["idempotent_retry"], true);
    assert_eq!(op_count(&temp), after_first);
    let conflicting_retry = output(
        &temp,
        "alice",
        &[
            "discuss",
            "post",
            "--topic",
            "planning",
            "--notify",
            "charlie",
            "--idempotency-key",
            "planning-post-1",
            "different content",
        ],
    );
    assert_eq!(conflicting_retry.status.code(), Some(3));
    assert_eq!(op_count(&temp), after_first);

    for actor in ["bob", "charlie"] {
        let notifications = json(
            &temp,
            actor,
            &["--json", "discuss", "notifications", "--topic", "planning"],
        );
        assert_eq!(notifications["posts"].as_array().unwrap().len(), 1);
        assert_eq!(notifications["posts"][0]["post_id"], first_post);
        assert_eq!(notifications["posts"][0]["public"], true);
    }
    for actor in ["alice", "dave"] {
        assert!(
            json(
                &temp,
                actor,
                &["--json", "discuss", "notifications", "--topic", "planning",],
            )["posts"]
                .as_array()
                .unwrap()
                .is_empty(),
            "sender exclusion and no prose mention parsing must hold for {actor}"
        );
    }

    let bob_status = json(&temp, "bob", &["--json", "actor", "status", "bob"]);
    assert_eq!(
        bob_status["attention"]["watched_topics"],
        serde_json::json!(["planning"])
    );
    assert_eq!(bob_status["attention"]["topic_notifications_unread"], 1);

    success(&temp, "bob", &["discuss", "unwatch", "planning"]);
    assert!(
        json(&temp, "bob", &["--json", "discuss", "watches"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    let second = json(
        &temp,
        "alice",
        &[
            "--json",
            "discuss",
            "post",
            "--topic",
            "planning",
            "--notify",
            "charlie",
            "--idempotency-key",
            "planning-post-2",
            "second public update",
        ],
    );
    let second_post = second["post_id"].as_str().unwrap().to_string();
    assert_eq!(
        second["notification_recipients"],
        serde_json::json!(["charlie"])
    );

    let bob_notifications = json(
        &temp,
        "bob",
        &["--json", "discuss", "notifications", "--topic", "planning"],
    );
    assert_eq!(bob_notifications["posts"].as_array().unwrap().len(), 1);
    assert_eq!(bob_notifications["posts"][0]["post_id"], first_post);
    let bob_unread = json(
        &temp,
        "bob",
        &[
            "--json", "discuss", "unread", "--page", "--topic", "planning",
        ],
    );
    assert_eq!(bob_unread["posts"].as_array().unwrap().len(), 2);

    let newest_page = json(
        &temp,
        "charlie",
        &[
            "--json",
            "discuss",
            "notifications",
            "--topic",
            "planning",
            "--limit",
            "1",
        ],
    );
    assert_eq!(newest_page["posts"][0]["post_id"], second_post);
    assert_eq!(newest_page["page"]["has_older"], true);
    let older_page = json(
        &temp,
        "charlie",
        &[
            "--json",
            "discuss",
            "notifications",
            "--topic",
            "planning",
            "--before",
            &second_post,
            "--limit",
            "1",
        ],
    );
    assert_eq!(older_page["posts"][0]["post_id"], first_post);

    success(
        &temp,
        "charlie",
        &[
            "discuss",
            "mark-read",
            "--topic",
            "planning",
            "--through",
            &second_post,
        ],
    );
    assert!(
        json(
            &temp,
            "charlie",
            &["--json", "discuss", "notifications", "--topic", "planning",],
        )["posts"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let alice_list = json(
        &temp,
        "alice",
        &["--json", "discuss", "list", "--topic", "planning"],
    );
    let dave_list = json(
        &temp,
        "dave",
        &["--json", "discuss", "list", "--topic", "planning"],
    );
    assert_eq!(alice_list, dave_list, "attention must not alter visibility");

    let events = success(
        &temp,
        "bob",
        &[
            "--json",
            "events",
            "--kind",
            "discussion",
            "--for-actor",
            "bob",
        ],
    );
    let events: Vec<Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "discussion.watched")
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "discussion.unwatched")
    );
    let notified_post = events
        .iter()
        .find(|event| event["data"]["post_id"] == first_post)
        .unwrap();
    assert_eq!(notified_post["data"]["public"], true);
    assert_eq!(
        notified_post["data"]["notification_recipients"],
        serde_json::json!(["bob", "charlie"])
    );
    assert!(
        !events
            .iter()
            .any(|event| event["data"]["post_id"] == second_post)
    );
}

#[test]
fn watch_rejects_unknown_topic_without_creating_membership_or_visibility_rules() {
    let temp = TempDir::new().unwrap();
    init(&temp);
    let output = output(&temp, "bob", &["discuss", "watch", "missing"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("topic missing does not exist")
    );
    assert!(
        json(&temp, "bob", &["--json", "discuss", "watches"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}
