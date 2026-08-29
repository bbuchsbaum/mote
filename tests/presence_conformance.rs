//! Completion gate for actor presence across replay and monitoring surfaces.

use std::process::{Command, Output};

use jiff::Timestamp;
use serde_json::Value;
use tempfile::TempDir;

use mote::actor_status::actor_status;
use mote::events::{EventFilter, accepted_events};
use mote::{reducer, repo::Store, watch};

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn output(temp: &TempDir, actor: &str, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(temp.path())
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .output()
        .unwrap()
}

fn success(temp: &TempDir, actor: &str, args: &[&str]) -> String {
    let output = output(temp, actor, args);
    assert!(
        output.status.success(),
        "mote {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn json(temp: &TempDir, actor: &str, args: &[&str]) -> Value {
    serde_json::from_str(&success(temp, actor, args)).unwrap()
}

fn export_value(output: &str, name: &str) -> String {
    let prefix = format!("export {name}='");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| value.strip_suffix('\''))
        .unwrap()
        .into()
}

fn find_actor<'a>(actors: &'a Value, actor: &str) -> &'a Value {
    actors
        .as_array()
        .unwrap()
        .iter()
        .find(|status| status["actor"] == actor)
        .unwrap()
}

fn assert_matches_reference(state: &mote::state::State, status: &Value) {
    let as_of: Timestamp = status["as_of_ts"].as_str().unwrap().parse().unwrap();
    let recent_window = status["recent_window_s"].as_u64().unwrap() as u32;
    let expected = serde_json::to_value(actor_status(
        state,
        status["actor"].as_str().unwrap(),
        Some("alice"),
        as_of,
        recent_window,
    ))
    .unwrap();
    assert_eq!(*status, expected);
}

#[test]
fn monitoring_surfaces_match_one_reference_projection_and_restart_cleanly() {
    let temp = TempDir::new().unwrap();
    success(&temp, "bootstrap", &["init"]);
    success(&temp, "alice", &["discuss", "topic", "new", "coordination"]);
    success(&temp, "bob", &["discuss", "watch", "coordination"]);

    let session_a = export_value(
        &success(
            &temp,
            "alice",
            &[
                "session", "start", "--as", "bob", "--ttl", "15m", "--label", "builder",
            ],
        ),
        "MOTE_SESSION",
    );
    let session_b = export_value(
        &success(
            &temp,
            "alice",
            &[
                "session", "start", "--as", "bob", "--ttl", "15m", "--label", "reviewer",
            ],
        ),
        "MOTE_SESSION",
    );
    success(
        &temp,
        "bob",
        &[
            "session",
            "status",
            "working",
            "--id",
            &session_a,
            "--message",
            "implementing",
        ],
    );
    success(
        &temp,
        "bob",
        &[
            "session",
            "status",
            "waiting",
            "--id",
            &session_b,
            "--message",
            "review queue",
        ],
    );
    let issue = success(&temp, "alice", &["new", "cross-surface gate"]);
    success(
        &temp,
        "alice",
        &[
            "msg", "send", "--to", "bob", "--issue", &issue, "--kind", "request", "inspect",
        ],
    );
    success(
        &temp,
        "alice",
        &[
            "discuss",
            "post",
            "--topic",
            "coordination",
            "--notify",
            "bob",
            "status update",
        ],
    );

    let store = Store::open(&temp.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();

    let direct = json(&temp, "alice", &["--json", "actor", "status", "bob"]);
    let list = json(&temp, "alice", &["--json", "actor", "list"]);
    let listed = &find_actor(&list, "bob")["status"];
    let board = json(&temp, "alice", &["--json", "board"]);
    let boarded = find_actor(&board["actors"], "bob");
    let in_flight = json(
        &temp,
        "alice",
        &["--json", "in-flight", "--no-git", "--minutes", "10"],
    );
    let flying = find_actor(&in_flight["actors"], "bob");

    for status in [&direct, listed, boarded, flying] {
        assert_matches_reference(&state, status);
        assert_eq!(status["presence"]["state"], "live");
        assert_eq!(status["presence"]["live_session_count"], 2);
        assert_eq!(
            status["intent"]["states"],
            serde_json::json!(["waiting", "working"])
        );
        assert_eq!(status["attention"]["inbox_unacked"], 1);
        assert_eq!(status["attention"]["topic_notifications_unread"], 1);
        assert_eq!(
            status["attention"]["watched_topics"],
            serde_json::json!(["coordination"])
        );
    }

    let board_as_of = board["as_of_ts"].as_str().unwrap();
    let watched = watch::snapshot_value(&state, Some("alice"), board_as_of);
    let watch_status = find_actor(&watched["actors"], "bob");
    assert_eq!(watch_status, boarded);
    assert_eq!(watched["ts"], board["as_of_ts"]);

    // Reopening and replaying from immutable ops must not change a fixed-time
    // projection or snapshot.
    let injected = "2035-01-01T00:00:00Z";
    let first_restart = watch::snapshot_value(&state, Some("alice"), injected);
    drop(store);
    let reopened = Store::open(&temp.path().join(".mote")).unwrap();
    let replayed = reducer::replay_store(&reopened).unwrap();
    let second_restart = watch::snapshot_value(&replayed, Some("alice"), injected);
    assert_eq!(first_restart, second_restart);

    // Stable event ids provide exact cursor resume after restart.
    let filter = EventFilter::new(&["all".into()], None).unwrap();
    let events = accepted_events(&reopened, None, &filter).unwrap();
    assert!(events.len() > 4);
    let cursor_index = events.len() / 2;
    let cursor = events[cursor_index].event_id.clone();
    let resumed = accepted_events(&reopened, Some(&cursor), &filter).unwrap();
    let expected: Vec<&str> = events
        .iter()
        .skip(cursor_index + 1)
        .map(|event| event.event_id.as_str())
        .collect();
    assert_eq!(
        resumed
            .iter()
            .map(|event| event.event_id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
}
