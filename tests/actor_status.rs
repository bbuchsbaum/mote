//! Conformance tests for the pure `mote.actor-status.v1` projection.

use std::collections::BTreeMap;
use std::process::Command;

use jiff::Timestamp;
use serde_json::Value;
use tempfile::TempDir;

use mote::actor_status::{ACTOR_STATUS_SCHEMA, actor_status};
use mote::op::{
    ScalarSet, Status, make_board_post, make_board_read, make_claim, make_create, make_msg_ack,
    make_msg_send, make_patch, make_reserve_open, make_session_heartbeat, make_session_start,
    make_session_status,
};
use mote::{publish, reducer, repo::Store};

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn init_store() -> (TempDir, Store) {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    (td, store)
}

fn put(store: &Store, op: &mote::op::Op) -> String {
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

#[test]
fn injected_time_derives_live_mixed_and_expired_without_making_heartbeat_work() {
    let (_td, store) = init_store();
    put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-a".into(),
            60,
            None,
            None,
            at("2030-01-01T00:00:00Z"),
        ),
    );
    put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-b".into(),
            60,
            None,
            None,
            at("2030-01-01T00:00:05Z"),
        ),
    );
    put(
        &store,
        &make_session_status(
            "alice".into(),
            "sess-a".into(),
            "working".into(),
            Some("parser".into()),
            None,
            None,
            at("2030-01-01T00:00:10Z"),
        ),
    );
    put(
        &store,
        &make_session_status(
            "alice".into(),
            "sess-b".into(),
            "waiting".into(),
            Some("review".into()),
            None,
            None,
            at("2030-01-01T00:00:11Z"),
        ),
    );
    let heartbeat = put(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            "sess-a".into(),
            60,
            None,
            at("2030-01-01T00:00:50Z"),
        ),
    );
    let state = reducer::replay_store(&store).unwrap();

    let before_heartbeat = actor_status(
        &state,
        "alice",
        Some("alice"),
        at("2030-01-01T00:00:20Z"),
        600,
    );
    assert_eq!(before_heartbeat.presence.state, "live");
    assert_eq!(before_heartbeat.presence.live_session_count, 2);
    assert_eq!(before_heartbeat.intent.states, ["waiting", "working"]);
    assert!(before_heartbeat.intent.mixed);
    assert_eq!(
        before_heartbeat.sessions[0].last_heartbeat_op_id,
        before_heartbeat.sessions[0].started_op_id
    );

    let one_live = actor_status(
        &state,
        "alice",
        Some("alice"),
        at("2030-01-01T00:01:06Z"),
        600,
    );
    assert_eq!(one_live.presence.live_session_count, 1);
    assert_eq!(one_live.intent.states, ["working"]);
    assert!(!one_live.intent.mixed);
    assert_eq!(one_live.sessions[0].last_heartbeat_op_id, heartbeat);
    assert_eq!(
        one_live.sessions[0].lease_until_ts,
        "2030-01-01T00:01:50.000000Z"
    );
    assert!(one_live.activity.last_work.is_none());
    assert!(one_live.activity.last_interaction.is_none());
    assert!(
        !one_live.activity.recent,
        "heartbeat is not substantive activity"
    );
    assert_eq!(
        one_live.activity.last_observed.as_ref().unwrap().event_type,
        "session.heartbeat"
    );

    let expired = actor_status(
        &state,
        "alice",
        Some("alice"),
        at("2030-01-01T00:01:50Z"),
        600,
    );
    assert_eq!(expired.presence.state, "expired");
    assert_eq!(expired.presence.reason, "ttl_elapsed");
    assert!(expired.intent.states.is_empty());
    assert!(expired.sessions.iter().all(|session| !session.live));
}

#[test]
fn recipient_only_is_untracked_until_ack_and_read_markers_are_interactions() {
    let (_td, store) = init_store();
    put(
        &store,
        &make_create(
            "worker".into(),
            "task".into(),
            ScalarSet {
                title: Some("task".into()),
                ..Default::default()
            },
            at("2030-01-01T01:00:00Z"),
        ),
    );
    put(
        &store,
        &make_msg_send(
            "sender".into(),
            "msg-1".into(),
            "recipient".into(),
            None,
            None,
            "note".into(),
            "hello".into(),
            at("2030-01-01T01:00:02Z"),
        ),
    );
    put(
        &store,
        &make_msg_ack(
            "recipient".into(),
            "msg-1".into(),
            at("2030-01-01T01:00:03Z"),
        ),
    );
    let post_op = put(
        &store,
        &make_board_post(
            "sender".into(),
            "post-1".into(),
            "general".into(),
            "announcement".into(),
            None,
            at("2030-01-01T01:00:04Z"),
        ),
    );
    put(
        &store,
        &make_board_read(
            "recipient".into(),
            post_op,
            None,
            at("2030-01-01T01:00:05Z"),
        ),
    );
    let state = reducer::replay_store(&store).unwrap();

    let recipient_only = actor_status(
        &state,
        "recipient",
        None,
        at("2030-01-01T01:00:02.500000Z"),
        10,
    );
    assert!(recipient_only.known);
    assert_eq!(recipient_only.presence.state, "untracked");
    assert!(recipient_only.activity.last_observed.is_none());

    let after_ack = actor_status(
        &state,
        "recipient",
        None,
        at("2030-01-01T01:00:03.500000Z"),
        10,
    );
    assert_eq!(after_ack.presence.state, "recent");
    assert_eq!(
        after_ack
            .activity
            .last_interaction
            .as_ref()
            .unwrap()
            .event_type,
        "message.acknowledged"
    );

    let after_read = actor_status(&state, "recipient", None, at("2030-01-01T01:00:05Z"), 10);
    assert_eq!(
        after_read
            .activity
            .last_interaction
            .as_ref()
            .unwrap()
            .event_type,
        "discussion.read"
    );

    let recent_worker = actor_status(&state, "worker", None, at("2030-01-01T01:00:05Z"), 10);
    assert_eq!(recent_worker.presence.state, "recent");
    let stale_worker = actor_status(&state, "worker", None, at("2030-01-01T01:00:11Z"), 10);
    assert_eq!(stale_worker.presence.state, "untracked");
}

#[test]
fn work_and_attention_reuse_tracker_and_mailbox_dispositions() {
    let (_td, store) = init_store();
    let create = put(
        &store,
        &make_create(
            "alice".into(),
            "task".into(),
            ScalarSet {
                title: Some("task".into()),
                ..Default::default()
            },
            at("2030-01-01T02:00:00Z"),
        ),
    );
    put(
        &store,
        &make_patch(
            "alice".into(),
            "task".into(),
            BTreeMap::from([("status".into(), create)]),
            ScalarSet {
                status: Some(Status::Doing),
                ..Default::default()
            },
            at("2030-01-01T02:00:01Z"),
        ),
    );
    put(
        &store,
        &make_claim(
            "alice".into(),
            "task".into(),
            "alice".into(),
            300,
            None,
            at("2030-01-01T02:00:02Z"),
        ),
    );
    put(
        &store,
        &make_reserve_open(
            "alice".into(),
            "rv-1".into(),
            "task".into(),
            vec!["src/parser.rs".into()],
            300,
            at("2030-01-01T02:00:03Z"),
        ),
    );
    put(
        &store,
        &make_msg_send(
            "bob".into(),
            "msg-request".into(),
            "alice".into(),
            Some("task".into()),
            None,
            "request".into(),
            "need a decision".into(),
            at("2030-01-01T02:00:04Z"),
        ),
    );
    put(
        &store,
        &make_board_post(
            "bob".into(),
            "post-attention".into(),
            "general".into(),
            "please inspect".into(),
            None,
            at("2030-01-01T02:00:05Z"),
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    let status = actor_status(
        &state,
        "alice",
        Some("alice"),
        at("2030-01-01T02:00:06Z"),
        600,
    );
    assert_eq!(status.work.active_claims, ["task"]);
    assert_eq!(status.work.active_reservations, ["rv-1"]);
    assert_eq!(status.work.doing_beads, ["task"]);
    assert_eq!(status.attention.inbox_unacked, 1);
    assert_eq!(status.attention.incoming_open_requests, 1);
    assert_eq!(status.attention.discussion_unread, 1);
}

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn run(td: &TempDir, args: &[&str]) -> String {
    let output = Command::new(mote_bin())
        .args(args)
        .current_dir(td.path())
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "`mote {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn cli_schema_is_stable_embedded_in_actor_list_and_passive_queries_append_nothing() {
    let td = TempDir::new().unwrap();
    run(&td, &["init"]);
    run(
        &td,
        &[
            "session", "start", "--as", "alice", "--ttl", "15m", "--actor", "alice",
        ],
    );
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let before = store.list_op_filenames().unwrap();

    let status: Value = serde_json::from_str(&run(
        &td,
        &["--json", "--actor", "alice", "actor", "status"],
    ))
    .unwrap();
    assert_eq!(status["schema"], ACTOR_STATUS_SCHEMA);
    assert_eq!(status["actor"], "alice");
    assert_eq!(status["known"], true);
    assert_eq!(status["current"], true);
    for key in [
        "presence",
        "activity",
        "sessions",
        "intent",
        "work",
        "attention",
    ] {
        assert!(status.get(key).is_some(), "missing {key}: {status}");
    }

    let actors: Value =
        serde_json::from_str(&run(&td, &["--json", "--actor", "alice", "actor", "list"])).unwrap();
    assert_eq!(actors[0]["status"]["schema"], ACTOR_STATUS_SCHEMA);
    let unknown: Value =
        serde_json::from_str(&run(&td, &["--json", "actor", "status", "never-seen"])).unwrap();
    assert_eq!(unknown["known"], false, "unknown status: {unknown}");
    assert_eq!(unknown["presence"]["state"], "untracked");
    assert_eq!(store.list_op_filenames().unwrap(), before);
}
