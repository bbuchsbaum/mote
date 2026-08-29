//! Cross-surface conformance for replay-derived actor monitoring.

use std::fs;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_store(temp: &TempDir) {
    let output = Command::new(mote_bin())
        .arg("init")
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_output(temp: &TempDir, actor: &str, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(temp.path())
        .output()
        .unwrap()
}

fn run(temp: &TempDir, actor: &str, args: &[&str]) -> String {
    let output = run_output(temp, actor, args);
    assert!(
        output.status.success(),
        "mote {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn run_json(temp: &TempDir, actor: &str, args: &[&str]) -> Value {
    serde_json::from_str(&run(temp, actor, args)).unwrap()
}

fn op_count(temp: &TempDir) -> usize {
    fs::read_dir(temp.path().join(".mote/ops"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

fn scenario() -> TempDir {
    let temp = TempDir::new().unwrap();
    init_store(&temp);
    let issue = run(&temp, "alice", &["new", "presence test"]);
    run(
        &temp,
        "alice",
        &["session", "start", "--as", "alice", "--ttl", "300"],
    );
    run(
        &temp,
        "alice",
        &[
            "msg", "send", "--to", "bob", "--issue", &issue, "--kind", "fyi", "hello",
        ],
    );
    temp
}

#[test]
fn actor_list_filters_presence_and_activity_without_losing_legacy_fields() {
    let temp = scenario();
    let before = op_count(&temp);

    let live = run_json(
        &temp,
        "alice",
        &["--json", "actor", "list", "--presence", "live"],
    );
    let live = live.as_array().unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0]["actor"], "alice");
    assert_eq!(live[0]["status"]["schema"], "mote.actor-status.v1");
    for legacy in [
        "current",
        "last_activity_ts",
        "last_activity_op_id",
        "active_claims",
        "active_reservations",
        "orphaned_claims",
        "orphaned_reservations",
        "inbox_unacked",
        "incoming_open_requests",
    ] {
        assert!(
            live[0].get(legacy).is_some(),
            "missing legacy field {legacy}"
        );
    }

    let untracked = run_json(
        &temp,
        "alice",
        &["--json", "actor", "list", "--presence", "untracked"],
    );
    let names: Vec<&str> = untracked
        .as_array()
        .unwrap()
        .iter()
        .map(|actor| actor["actor"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bob"]);

    let active = run_json(
        &temp,
        "alice",
        &["--json", "actor", "list", "--active-within", "600"],
    );
    assert_eq!(active.as_array().unwrap().len(), 1);
    assert_eq!(active[0]["actor"], "alice");

    let invalid = run_output(&temp, "alice", &["actor", "list", "--presence", "online"]);
    assert_eq!(invalid.status.code(), Some(3));
    assert_eq!(
        op_count(&temp),
        before,
        "actor monitoring must be read-only"
    );
}

#[test]
fn board_in_flight_and_status_share_explicit_actor_schema_and_are_read_only() {
    let temp = scenario();
    let before = op_count(&temp);

    let board = run_json(&temp, "alice", &["--json", "board"]);
    for legacy in [
        "actor",
        "status_counts",
        "active_claims",
        "active_reservations",
        "inbox_unacked",
        "discussion_unread",
    ] {
        assert!(board.get(legacy).is_some(), "missing board field {legacy}");
    }
    let board_as_of = board["as_of_ts"].as_str().unwrap();
    for actor in board["actors"].as_array().unwrap() {
        assert_eq!(actor["schema"], "mote.actor-status.v1");
        assert_eq!(actor["as_of_ts"], board_as_of);
    }

    let in_flight = run_json(&temp, "alice", &["--json", "in-flight", "--no-git"]);
    for legacy in [
        "sessions",
        "reservations",
        "doing",
        "topics",
        "candidates",
        "recent_commits_advisory",
    ] {
        assert!(
            in_flight.get(legacy).is_some(),
            "missing in-flight field {legacy}"
        );
    }
    let in_flight_as_of = in_flight["now_ts"].as_str().unwrap();
    for actor in in_flight["actors"].as_array().unwrap() {
        assert_eq!(actor["schema"], "mote.actor-status.v1");
        assert_eq!(actor["as_of_ts"], in_flight_as_of);
    }

    let status = run(&temp, "alice", &["actor", "status"]);
    assert!(status.contains("identity:    source=flag"));
    assert!(status.contains("presence:    live source=session_lease reason=lease_valid as-of="));
    for output in [
        status,
        run(&temp, "alice", &["board"]),
        run(&temp, "alice", &["in-flight", "--no-git"]),
    ] {
        assert!(output.contains("source="));
        assert!(output.contains("reason="));
        assert!(output.contains("as-of="));
        assert!(!output.to_ascii_lowercase().contains("online"));
    }

    assert_eq!(
        op_count(&temp),
        before,
        "board, in-flight, and actor status must be read-only"
    );
}
