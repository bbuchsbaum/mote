use std::process::{Command, Output};

use jiff::Timestamp;
use mote::events::{EventFilter, EventTailer};
use mote::ids;
use mote::op::{
    ScalarSet, make_board_post, make_create, make_msg_ack, make_msg_send,
    make_msg_send_with_metadata,
};
use mote::publish;
use mote::reducer;
use mote::repo::Store;
use mote::state::RequestState;
use mote::watch;
use tempfile::TempDir;

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn request(store: &Store, msg_id: &str, sent_at: &str) {
    publish::publish_op(
        store,
        &make_msg_send(
            "alice".into(),
            msg_id.into(),
            "bob".into(),
            None,
            None,
            "request".into(),
            "please review".into(),
            at(sent_at),
        ),
    )
    .unwrap();
}

fn message_filter(horizon_s: u32) -> EventFilter {
    EventFilter::new(&["message".into()], Some("bob".into()))
        .unwrap()
        .with_request_stale_after(horizon_s)
}

#[test]
fn injected_clock_emits_one_stable_cursor_at_the_configured_horizon() {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    let msg_id = ids::new_msg_id();
    request(&store, &msg_id, "2030-01-01T00:00:00Z");
    let filter = message_filter(3600);
    let mut tailer = EventTailer::new(&store, None, 1).unwrap();

    assert!(
        tailer
            .poll_at(&store, &filter, at("2030-01-01T00:59:59Z"))
            .unwrap()
            .is_empty()
    );
    let warning = tailer
        .poll_at(&store, &filter, at("2030-01-01T01:00:00Z"))
        .unwrap();
    assert_eq!(warning.len(), 1);
    let warning = &warning[0];
    assert_eq!(warning.event_type, "request.stale");
    assert_eq!(warning.category, "message");
    assert_eq!(warning.actor, "bob");
    assert_eq!(warning.data["msg_id"], msg_id);
    assert_eq!(warning.data["request_state"], "open");
    assert_eq!(warning.data["stale_after_s"], 3600);
    assert_eq!(
        warning.data["stale_since_ts"],
        "2030-01-01T01:00:00.000000Z"
    );
    assert_eq!(warning.data["acknowledgement_is_fulfillment"], false);
    assert!(warning.event_id.contains("-d-request-stale-3600-"));
    assert!(
        tailer
            .poll_at(&store, &filter, at("2030-01-01T02:00:00Z"))
            .unwrap()
            .is_empty(),
        "one tailer emits a derived boundary once"
    );

    let mut fresh = EventTailer::new(&store, None, 1).unwrap();
    let repeated = fresh
        .poll_at(&store, &filter, at("2030-01-01T03:00:00Z"))
        .unwrap();
    assert_eq!(repeated[0].event_id, warning.event_id);
    assert_eq!(repeated[0].ts, warning.ts);

    let mut resumed = EventTailer::new(&store, Some(&warning.event_id), 1).unwrap();
    assert!(
        resumed
            .poll_at(&store, &filter, at("2030-01-01T03:00:00Z"))
            .unwrap()
            .is_empty(),
        "the derived event id is a stable resume cursor"
    );

    let two_hour = message_filter(7200);
    let mut configured = EventTailer::new(&store, None, 1).unwrap();
    assert!(
        configured
            .poll_at(&store, &two_hour, at("2030-01-01T01:59:59Z"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        configured
            .poll_at(&store, &two_hour, at("2030-01-01T02:00:00Z"))
            .unwrap()[0]
            .data["stale_after_s"],
        7200
    );
}

#[test]
fn acknowledgement_and_unrelated_posts_do_not_answer_but_response_and_decline_do() {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    let responded_id = ids::new_msg_id();
    let declined_id = ids::new_msg_id();
    request(&store, &responded_id, "2030-01-01T00:00:00Z");
    request(&store, &declined_id, "2030-01-01T00:00:01Z");

    publish::publish_op(
        &store,
        &make_msg_ack(
            "bob".into(),
            responded_id.clone(),
            at("2030-01-01T00:10:00Z"),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_board_post(
            "bob".into(),
            ids::new_post_id(),
            "coordination".into(),
            "I posted elsewhere but did not link an answer".into(),
            None,
            at("2030-01-01T00:20:00Z"),
        ),
    )
    .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.messages[&responded_id].request_state,
        Some(RequestState::Open)
    );
    let snapshot = watch::snapshot_value_with_request_horizon(
        &state,
        Some("bob"),
        "2030-01-01T01:00:01Z",
        3600,
    );
    assert_eq!(snapshot["stale_open_requests"].as_array().unwrap().len(), 2);
    let acknowledged = snapshot["stale_open_requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["msg_id"] == responded_id)
        .unwrap();
    assert_eq!(acknowledged["acknowledged"], true);
    assert_eq!(acknowledged["request_state"], "open");
    assert!(
        watch::snapshot_value_with_request_horizon(
            &state,
            Some("alice"),
            "2030-01-01T01:00:01Z",
            3600,
        )["stale_open_requests"]
            .as_array()
            .unwrap()
            .is_empty(),
        "watch directs warnings to the request recipient"
    );

    publish::publish_op(
        &store,
        &make_msg_send_with_metadata(
            "bob".into(),
            ids::new_msg_id(),
            "alice".into(),
            None,
            None,
            "response".into(),
            "reviewed".into(),
            Some(responded_id.clone()),
            Some(responded_id.clone()),
            None,
            at("2030-01-01T01:10:00Z"),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_msg_send_with_metadata(
            "bob".into(),
            ids::new_msg_id(),
            "alice".into(),
            None,
            None,
            "decline".into(),
            "cannot take this".into(),
            Some(declined_id.clone()),
            Some(declined_id.clone()),
            None,
            at("2030-01-01T01:10:01Z"),
        ),
    )
    .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.messages[&responded_id].request_state,
        Some(RequestState::Responded)
    );
    assert_eq!(
        state.messages[&declined_id].request_state,
        Some(RequestState::Declined)
    );
    assert!(
        watch::snapshot_value_with_request_horizon(
            &state,
            Some("bob"),
            "2030-01-01T03:00:00Z",
            3600,
        )["stale_open_requests"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut tailer = EventTailer::new(&store, None, 1).unwrap();
    assert!(
        tailer
            .poll_at(&store, &message_filter(3600), at("2030-01-01T03:00:00Z"))
            .unwrap()
            .is_empty(),
        "responded and declined requests have no derived stale event"
    );
}

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn run(td: &TempDir, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .current_dir(td.path())
        .output()
        .unwrap()
}

#[test]
fn stateful_cli_warns_the_recipient_and_ack_does_not_silence_it() {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    let bead = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            bead.clone(),
            ScalarSet {
                title: Some("old request".into()),
                ..Default::default()
            },
            at("2020-01-01T00:00:00Z"),
        ),
    )
    .unwrap();
    let msg_id = ids::new_msg_id();
    publish::publish_op(
        &store,
        &make_msg_send(
            "alice".into(),
            msg_id.clone(),
            "bob".into(),
            Some(bead.clone()),
            None,
            "request".into(),
            "old request".into(),
            at("2020-01-01T00:00:01Z"),
        ),
    )
    .unwrap();

    let read_only = run(&td, &["show", &bead, "--actor", "bob"]);
    assert!(read_only.status.success());
    assert!(!String::from_utf8_lossy(&read_only.stderr).contains("stale open"));

    let stateful = run(
        &td,
        &[
            "note",
            &bead,
            "--kind",
            "progress",
            "working",
            "--actor",
            "bob",
            "--request-stale-after",
            "1h",
        ],
    );
    assert!(stateful.status.success());
    let warning = String::from_utf8(stateful.stderr).unwrap();
    assert!(
        warning.contains(&format!("request {msg_id} from alice")),
        "{warning}"
    );
    assert!(warning.contains("acknowledgement records receipt only"));
    assert!(warning.contains("mote msg reply"));

    let ack = run(&td, &["msg", "ack", &msg_id, "--actor", "bob"]);
    assert!(ack.status.success());
    assert!(String::from_utf8_lossy(&ack.stderr).contains(&msg_id));

    let still_open = run(
        &td,
        &[
            "note",
            &bead,
            "--kind",
            "progress",
            "still working",
            "--actor",
            "bob",
        ],
    );
    assert!(still_open.status.success());
    assert!(String::from_utf8_lossy(&still_open.stderr).contains("acknowledged=true"));

    let reply = run(&td, &["msg", "reply", &msg_id, "done", "--actor", "bob"]);
    assert!(reply.status.success());
    let after = run(
        &td,
        &[
            "note",
            &bead,
            "--kind",
            "progress",
            "after reply",
            "--actor",
            "bob",
        ],
    );
    assert!(after.status.success());
    assert!(
        !String::from_utf8_lossy(&after.stderr).contains("request "),
        "response must suppress later stale warnings: {}",
        String::from_utf8_lossy(&after.stderr)
    );
}
