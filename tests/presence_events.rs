//! Cursor and clock semantics for actor-level presence transition events.

use jiff::Timestamp;
use std::time::Duration;
use tempfile::TempDir;

use mote::events::{EventFilter, EventTailer, accepted_events};
use mote::op::{make_session_end, make_session_heartbeat, make_session_start, make_session_status};
use mote::{publish, repo::Store};

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn init_store() -> (TempDir, Store) {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    (td, store)
}

fn put(store: &Store, op: &mote::op::Op) -> String {
    publish::publish_op(store, op).unwrap().into_string()
}

#[test]
fn raw_session_events_and_actor_presence_transitions_are_separately_filterable() {
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
        &make_session_heartbeat(
            "alice".into(),
            "sess-a".into(),
            60,
            None,
            at("2030-01-01T00:00:01Z"),
        ),
    );
    put(
        &store,
        &make_session_status(
            "alice".into(),
            "sess-a".into(),
            "working".into(),
            None,
            None,
            None,
            at("2030-01-01T00:00:02Z"),
        ),
    );
    put(
        &store,
        &make_session_end("alice".into(), "sess-a".into(), at("2030-01-01T00:00:03Z")),
    );

    let session = EventFilter::new(&["session".into()], None).unwrap();
    let session_types: Vec<String> = accepted_events(&store, None, &session)
        .unwrap()
        .into_iter()
        .map(|event| event.event_type)
        .collect();
    assert_eq!(
        session_types,
        [
            "session.started",
            "session.heartbeat",
            "session.status_changed",
            "session.ended"
        ]
    );

    let presence = EventFilter::new(&["presence".into()], None).unwrap();
    let events = accepted_events(&store, None, &presence).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].event_type, "presence.live");
    assert_eq!(events[1].event_type, "presence.ended");
    assert!(events.iter().all(|event| event.category == "presence"));
    assert_eq!(events[0].data["presence_state"], "live");
    assert_eq!(events[0].data["source"], "session_lease");
    assert_eq!(events[0].data["reason"], "lease_valid");
    assert_eq!(events[0].data["as_of_ts"], events[0].ts);
    assert_eq!(events[1].data["presence_state"], "expired");
    assert_eq!(events[1].data["reason"], "ended");
    assert_ne!(events[0].event_id, events[0].op_id);

    let other = EventFilter::new(&["presence".into()], Some("bob".into())).unwrap();
    assert!(accepted_events(&store, None, &other).unwrap().is_empty());
}

#[test]
fn derived_warning_and_expiry_are_exactly_once_cursorable_without_new_ops() {
    let (_td, store) = init_store();
    put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-a".into(),
            10,
            None,
            None,
            at("2030-01-01T01:00:00Z"),
        ),
    );
    let op_count = store.list_op_filenames().unwrap().len();
    let filter = EventFilter::new(&["presence".into()], None).unwrap();
    let mut tailer = EventTailer::new(&store, None, 1).unwrap();

    let warning = tailer
        .poll_at(&store, &filter, at("2030-01-01T01:00:09Z"))
        .unwrap();
    assert_eq!(warning.len(), 1);
    assert_eq!(warning[0].event_type, "presence.expiring");
    assert_eq!(warning[0].ts, "2030-01-01T01:00:09.000000Z");
    assert_eq!(warning[0].data["as_of_ts"], "2030-01-01T01:00:09.000000Z");
    assert_eq!(warning[0].data["deadline"], "2030-01-01T01:00:10.000000Z");
    assert!(
        tailer
            .poll_at(&store, &filter, at("2030-01-01T01:00:09Z"))
            .unwrap()
            .is_empty()
    );

    let mut resumed = EventTailer::new(&store, Some(&warning[0].event_id), 1).unwrap();
    assert!(
        resumed
            .poll_at(&store, &filter, at("2030-01-01T01:00:09Z"))
            .unwrap()
            .is_empty()
    );
    let expired = resumed
        .poll_at(&store, &filter, at("2030-01-01T01:00:10Z"))
        .unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].event_type, "presence.expired");
    assert_eq!(expired[0].data["reason"], "ttl_elapsed");
    assert_eq!(expired[0].data["as_of_ts"], "2030-01-01T01:00:10.000000Z");
    assert_eq!(store.list_op_filenames().unwrap().len(), op_count);
    assert!(
        resumed
            .poll_at(&store, &filter, at("2030-01-01T01:00:10Z"))
            .unwrap()
            .is_empty()
    );
    let mut after = EventTailer::new(&store, Some(&expired[0].event_id), 1).unwrap();
    assert!(
        after
            .poll_at(&store, &filter, at("2030-01-01T01:00:10Z"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn polling_after_the_deadline_reports_expired_without_replaying_a_missed_warning() {
    let (_td, store) = init_store();
    put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-a".into(),
            10,
            None,
            None,
            at("2030-01-01T02:00:00Z"),
        ),
    );
    let filter = EventFilter::new(&["presence".into()], None).unwrap();
    let mut tailer = EventTailer::new(&store, None, 1).unwrap();
    let events = tailer
        .poll_at(&store, &filter, at("2030-01-01T02:00:15Z"))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "presence.expired");
}

#[test]
fn fallback_tick_can_deliver_expiry_without_a_filesystem_write() {
    let (_td, store) = init_store();
    put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-a".into(),
            10,
            None,
            None,
            at("2030-01-01T02:30:00Z"),
        ),
    );
    let op_count = store.list_op_filenames().unwrap().len();
    let filter = EventFilter::new(&["presence".into()], None).unwrap();
    let mut tailer = EventTailer::new(&store, None, 1).unwrap();
    tailer.start(&store).unwrap();
    assert!(tailer.wait_timeout(Duration::from_secs(2)));
    let events = tailer
        .poll_at(&store, &filter, at("2030-01-01T02:30:10Z"))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "presence.expired");
    assert_eq!(store.list_op_filenames().unwrap().len(), op_count);
}

#[test]
fn explicit_end_of_the_final_live_session_suppresses_ttl_expiry() {
    let (_td, store) = init_store();
    put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-a".into(),
            10,
            None,
            None,
            at("2030-01-01T03:00:00Z"),
        ),
    );
    put(
        &store,
        &make_session_end("alice".into(), "sess-a".into(), at("2030-01-01T03:00:05Z")),
    );
    let filter = EventFilter::new(&["presence".into()], None).unwrap();
    let mut tailer = EventTailer::new(&store, None, 1).unwrap();
    assert!(
        tailer
            .poll_at(&store, &filter, at("2030-01-01T03:00:10Z"))
            .unwrap()
            .is_empty()
    );
    let one_shot = accepted_events(&store, None, &filter).unwrap();
    assert_eq!(
        one_shot
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["presence.live", "presence.ended"]
    );
}

#[test]
fn resume_after_raw_cursor_delivers_the_later_synthetic_transition_once() {
    let (_td, store) = init_store();
    let start_op = put(
        &store,
        &make_session_start(
            "alice".into(),
            "sess-a".into(),
            60,
            None,
            None,
            at("2030-01-01T04:00:00Z"),
        ),
    );
    let filter = EventFilter::new(&["presence".into()], Some("alice".into())).unwrap();
    let resumed = accepted_events(&store, Some(&start_op), &filter).unwrap();
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].event_type, "presence.live");
    assert!(resumed[0].event_id.as_str() > start_op.as_str());

    let mut tailer = EventTailer::new(&store, Some(&start_op), 1).unwrap();
    let first = tailer
        .poll_at(&store, &filter, at("2030-01-01T04:00:01Z"))
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].event_id, resumed[0].event_id);
    assert!(
        tailer
            .poll_at(&store, &filter, at("2030-01-01T04:00:01Z"))
            .unwrap()
            .is_empty()
    );
}
