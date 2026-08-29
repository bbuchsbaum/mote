//! Explicit heartbeat and session-scoped intent protocol coverage.

use std::process::{Command, Output};

use jiff::Timestamp;
use serde_json::Value;
use tempfile::TempDir;

use mote::events::{self, EventFilter};
use mote::op::{make_session_end, make_session_heartbeat, make_session_start, make_session_status};
use mote::{publish, reducer, repo::Store};

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn init_store() -> (TempDir, Store) {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    (td, store)
}

fn accepted(store: &Store, op: &mote::op::Op) -> String {
    let name = publish::publish_op(store, op).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(
        state.was_accepted(name.as_str()),
        "{} rejected: {:?}",
        op.kind_name(),
        state.rejection_reason(name.as_str())
    );
    name.into_string()
}

fn rejected(store: &Store, op: &mote::op::Op, reason: &str) -> String {
    let name = publish::publish_op(store, op).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    let actual = state.rejection_reason(name.as_str()).unwrap();
    assert!(
        actual.contains(reason),
        "reason `{actual}` lacks `{reason}`"
    );
    name.into_string()
}

#[test]
fn injected_clock_boundaries_allow_expired_resume_but_never_ended_resurrection() {
    let (_td, store) = init_store();
    let session = "sess-boundary".to_string();
    let start = accepted(
        &store,
        &make_session_start(
            "alice".into(),
            session.clone(),
            60,
            Some("boundary test".into()),
            None,
            at("2030-01-01T00:00:00Z"),
        ),
    );

    rejected(
        &store,
        &make_session_status(
            "alice".into(),
            session.clone(),
            "working".into(),
            None,
            None,
            None,
            at("2030-01-01T00:01:00Z"),
        ),
        "is expired",
    );
    let heartbeat = accepted(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            session.clone(),
            60,
            Some("resume-1".into()),
            at("2030-01-01T00:01:01Z"),
        ),
    );
    let status = accepted(
        &store,
        &make_session_status(
            "alice".into(),
            session.clone(),
            "working".into(),
            Some("resumed work".into()),
            None,
            Some("status-1".into()),
            at("2030-01-01T00:01:02Z"),
        ),
    );
    accepted(
        &store,
        &make_session_end("alice".into(), session.clone(), at("2030-01-01T00:01:03Z")),
    );
    rejected(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            session.clone(),
            300,
            None,
            at("2030-01-01T00:01:04Z"),
        ),
        "has ended",
    );

    let state = reducer::replay_store(&store).unwrap();
    let record = &state.sessions[&session];
    assert_eq!(record.started_op_id, start);
    assert_eq!(record.last_heartbeat_op_id, heartbeat);
    assert_eq!(record.last_heartbeat_ts, "2030-01-01T00:01:01.000000Z");
    assert_eq!(record.lease_until_ts, "2030-01-01T00:02:01.000000Z");
    assert_eq!(record.intent.as_ref().unwrap().set_op_id, status);
    assert_eq!(record.intent.as_ref().unwrap().state, "working");
    assert!(record.ended_ts.is_some());
    assert!(!record.is_live("2030-01-01T00:01:04.000000Z"));
}

#[test]
fn intents_are_session_scoped_and_ownership_is_enforced_with_shared_actor_names() {
    let (_td, store) = init_store();
    for (id, second) in [("sess-a", 0), ("sess-b", 1)] {
        accepted(
            &store,
            &make_session_start(
                "shared".into(),
                id.into(),
                900,
                None,
                None,
                at(&format!("2030-01-01T01:00:0{second}Z")),
            ),
        );
    }
    accepted(
        &store,
        &make_session_status(
            "shared".into(),
            "sess-a".into(),
            "working".into(),
            Some("parser".into()),
            None,
            None,
            at("2030-01-01T01:00:02Z"),
        ),
    );
    accepted(
        &store,
        &make_session_status(
            "shared".into(),
            "sess-b".into(),
            "waiting".into(),
            Some("review".into()),
            None,
            None,
            at("2030-01-01T01:00:03Z"),
        ),
    );
    rejected(
        &store,
        &make_session_heartbeat(
            "intruder".into(),
            "sess-a".into(),
            900,
            None,
            at("2030-01-01T01:00:04Z"),
        ),
        "belongs to shared, not intruder",
    );
    rejected(
        &store,
        &make_session_status(
            "intruder".into(),
            "sess-b".into(),
            "away".into(),
            None,
            None,
            None,
            at("2030-01-01T01:00:05Z"),
        ),
        "belongs to shared, not intruder",
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.sessions["sess-a"].intent.as_ref().unwrap().state,
        "working"
    );
    assert_eq!(
        state.sessions["sess-b"].intent.as_ref().unwrap().state,
        "waiting"
    );
}

#[test]
fn retry_keys_do_not_extend_twice_or_emit_duplicate_session_events() {
    let (_td, store) = init_store();
    accepted(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-retry".into(),
            60,
            None,
            None,
            at("2030-01-01T02:00:00Z"),
        ),
    );
    let first = accepted(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            "sess-retry".into(),
            300,
            Some("heartbeat-1".into()),
            at("2030-01-01T02:00:10Z"),
        ),
    );
    rejected(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            "sess-retry".into(),
            300,
            Some("heartbeat-1".into()),
            at("2030-01-01T02:00:20Z"),
        ),
        "idempotent retry already accepted",
    );
    rejected(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            "sess-retry".into(),
            600,
            Some("heartbeat-1".into()),
            at("2030-01-01T02:00:30Z"),
        ),
        "different session action",
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.sessions["sess-retry"].last_heartbeat_op_id, first);
    assert_eq!(
        state.sessions["sess-retry"].lease_until_ts,
        "2030-01-01T02:05:10.000000Z"
    );
    let filter = EventFilter::new(&["session".into()], None).unwrap();
    let events = events::accepted_events(&store, None, &filter).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "session.heartbeat")
            .count(),
        1
    );
}

#[test]
fn legacy_repeated_start_replays_as_heartbeat_and_old_shape_gets_provenance_defaults() {
    let (_td, store) = init_store();
    let first = publish::publish_value(
        &store,
        serde_json::json!({
            "kind": "session_start",
            "actor": "legacy",
            "session_id": "sess-legacy",
            "ttl_s": 60,
            "label": "old client",
            "pid": 42,
        }),
        at("2030-01-01T03:00:00Z"),
    )
    .unwrap();
    let renewal = publish::publish_value(
        &store,
        serde_json::json!({
            "kind": "session_start",
            "actor": "legacy",
            "session_id": "sess-legacy",
            "ttl_s": 120,
        }),
        at("2030-01-01T03:00:30Z"),
    )
    .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    let session = &state.sessions["sess-legacy"];
    assert_eq!(session.started_op_id, first.as_str());
    assert_eq!(session.last_heartbeat_op_id, renewal.as_str());
    assert!(session.intent.is_none());

    let filter = EventFilter::new(&["session".into()], None).unwrap();
    let events = events::accepted_events(&store, None, &filter).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "session.started");
    assert_eq!(events[1].event_type, "session.heartbeat");
    assert_eq!(events[1].data["last_heartbeat_op_id"], renewal.as_str());
    assert_eq!(
        events[1].data["lease_until_ts"],
        "2030-01-01T03:02:30.000000Z"
    );
}

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn cli(td: &TempDir, actor: &str, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(td.path())
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .output()
        .unwrap()
}

fn cli_ok(td: &TempDir, actor: &str, args: &[&str]) -> String {
    let output = cli(td, actor, args);
    assert!(
        output.status.success(),
        "`mote {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn export_value(stdout: &str, name: &str) -> String {
    let prefix = format!("export {name}='");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|value| value.strip_suffix('\''))
        .unwrap()
        .to_string()
}

#[test]
fn cli_budget_skips_healthy_lease_and_retry_returns_without_appending() {
    let td = TempDir::new().unwrap();
    cli_ok(&td, "bootstrap", &["init"]);
    let activation = cli_ok(
        &td,
        "alice",
        &["session", "start", "--as", "alice", "--ttl", "1h"],
    );
    let session = export_value(&activation, "MOTE_SESSION");
    let store = Store::open(&td.path().join(".mote")).unwrap();
    assert_eq!(store.list_op_filenames().unwrap().len(), 1);

    let skipped = cli_ok(
        &td,
        "alice",
        &["--json", "session", "heartbeat", "--id", &session],
    );
    let skipped: Value = serde_json::from_str(&skipped).unwrap();
    assert_eq!(skipped["published"], false);
    assert_eq!(skipped["reason"], "outside_renewal_margin");
    assert_eq!(store.list_op_filenames().unwrap().len(), 1);

    let args = [
        "--json",
        "session",
        "heartbeat",
        "--id",
        &session,
        "--force",
        "--idempotency-key",
        "cli-heartbeat-1",
    ];
    let first: Value = serde_json::from_str(&cli_ok(&td, "alice", &args)).unwrap();
    assert_eq!(first["published"], true);
    assert_eq!(store.list_op_filenames().unwrap().len(), 2);
    let retry: Value = serde_json::from_str(&cli_ok(&td, "alice", &args)).unwrap();
    assert_eq!(retry["published"], false);
    assert_eq!(retry["idempotent_replay"], true);
    assert_eq!(retry["op_id"], first["op_id"]);
    assert_eq!(store.list_op_filenames().unwrap().len(), 2);

    let issue = cli_ok(&td, "alice", &["new", "heartbeat implementation"]);
    assert_eq!(store.list_op_filenames().unwrap().len(), 3);

    let status_args = vec![
        "--json",
        "session",
        "status",
        "working",
        "--id",
        &session,
        "--message",
        "heartbeat tests",
        "--issue",
        issue.as_str(),
        "--idempotency-key",
        "cli-status-1",
    ];
    let status: Value = serde_json::from_str(&cli_ok(&td, "alice", &status_args)).unwrap();
    assert_eq!(status["intent"]["state"], "working");
    assert_eq!(status["intent"]["issue"], issue);
    assert_eq!(store.list_op_filenames().unwrap().len(), 4);
    let retry: Value = serde_json::from_str(&cli_ok(&td, "alice", &status_args)).unwrap();
    assert_eq!(retry["idempotent_replay"], true);
    assert_eq!(store.list_op_filenames().unwrap().len(), 4);

    let filter = EventFilter::new(&["session".into()], None).unwrap();
    let event_types: Vec<String> = events::accepted_events(&store, None, &filter)
        .unwrap()
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert_eq!(
        event_types,
        vec![
            "session.started",
            "session.heartbeat",
            "session.status_changed"
        ]
    );
}
