use std::process::{Command, Output};

use jiff::Timestamp;
use serde_json::Value;
use tempfile::TempDir;

use mote::op::make_msg_send;
use mote::publish;
use mote::reducer;
use mote::repo::Store;
use mote::state::RequestState;

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

fn run_ok(td: &TempDir, args: &[&str]) -> String {
    let out = run(td, args);
    assert!(
        out.status.success(),
        "command failed: mote {}\nstdout={}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn init_store(td: &TempDir) {
    run_ok(td, &["init"]);
}

fn new_issue(td: &TempDir) -> String {
    run_ok(td, &["new", "request lifecycle", "--actor", "alice"])
}

#[test]
fn request_response_ack_and_resolution_are_distinct() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_issue(&td);

    let send_args = [
        "msg",
        "send",
        "--to",
        "bob",
        "--issue",
        &issue,
        "--kind",
        "request",
        "--idempotency-key",
        "request-42",
        "Release PRD.md",
        "--actor",
        "alice",
    ];
    let request_id = run_ok(&td, &send_args);
    assert_eq!(run_ok(&td, &send_args), request_id);

    // Receipt acknowledgement removes the inbox item but does not fulfill it.
    run_ok(&td, &["msg", "ack", &request_id, "--actor", "bob"]);
    let state = reducer::replay_store(&Store::open(&td.path().join(".mote")).unwrap()).unwrap();
    let request = &state.messages[&request_id];
    assert_eq!(state.messages.len(), 1, "idempotent retry duplicated send");
    assert_eq!(request.request_state, Some(RequestState::Open));
    assert!(request.ack_op_id.is_some());
    assert_eq!(request.correlation_id.as_deref(), Some(request_id.as_str()));

    let open: Value = serde_json::from_str(&run_ok(
        &td,
        &[
            "--json", "--actor", "bob", "msg", "requests", "--state", "open",
        ],
    ))
    .unwrap();
    assert_eq!(open.as_array().unwrap().len(), 1);

    let reply_args = [
        "msg",
        "reply",
        &request_id,
        "--kind",
        "response",
        "--idempotency-key",
        "response-42",
        "Released PRD.md",
        "--actor",
        "bob",
    ];
    let reply_id = run_ok(&td, &reply_args);
    assert_eq!(run_ok(&td, &reply_args), reply_id);

    let state = reducer::replay_store(&Store::open(&td.path().join(".mote")).unwrap()).unwrap();
    let request = &state.messages[&request_id];
    let reply = &state.messages[&reply_id];
    assert_eq!(state.messages.len(), 2, "idempotent reply was duplicated");
    assert_eq!(request.request_state, Some(RequestState::Responded));
    assert_eq!(request.response_msg_id.as_deref(), Some(reply_id.as_str()));
    assert_eq!(reply.reply_to.as_deref(), Some(request_id.as_str()));
    assert_eq!(reply.correlation_id, request.correlation_id);
    assert_eq!(reply.entity.as_deref(), Some(issue.as_str()));
    let alice_inbox: Value =
        serde_json::from_str(&run_ok(&td, &["--json", "--actor", "alice", "inbox"])).unwrap();
    assert_eq!(alice_inbox[0]["msg_id"], reply_id);
    assert_eq!(alice_inbox[0]["reply_to"], request_id);

    let wrong_actor = run(&td, &["msg", "resolve", &request_id, "--actor", "bob"]);
    assert_eq!(wrong_actor.status.code(), Some(2));
    run_ok(&td, &["msg", "resolve", &request_id, "--actor", "alice"]);

    let resolved: Value = serde_json::from_str(&run_ok(
        &td,
        &[
            "--json", "--actor", "alice", "msg", "requests", "--state", "resolved",
        ],
    ))
    .unwrap();
    let row = &resolved.as_array().unwrap()[0];
    assert_eq!(row["request_state"], "resolved");
    assert_eq!(row["response_msg_id"], reply_id);

    let events = run_ok(&td, &["--json", "events", "--kind", "message"]);
    let message_events: Vec<Value> = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect();
    let opened = message_events
        .iter()
        .find(|event| event["type"] == "message.sent" && event["data"]["msg_id"] == request_id)
        .expect("request event");
    assert_eq!(opened["data"]["request_state"], "open");
    let responded = message_events
        .iter()
        .find(|event| event["type"] == "message.responded")
        .expect("response event");
    assert_eq!(responded["data"]["reply_to"], request_id);
    assert_eq!(responded["data"]["request_state"], "responded");
    let resolved_event = message_events
        .iter()
        .find(|event| event["type"] == "message.resolved")
        .expect("resolve event");
    assert_eq!(resolved_event["data"]["msg_id"], request_id);
    assert_eq!(resolved_event["data"]["request_state"], "resolved");
}

#[test]
fn decline_permissions_and_idempotency_conflicts_are_enforced() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_issue(&td);
    let request_id = run_ok(
        &td,
        &[
            "msg",
            "send",
            "--to",
            "bob",
            "--issue",
            &issue,
            "--kind",
            "request",
            "--idempotency-key",
            "request-1",
            "Take this",
            "--actor",
            "alice",
        ],
    );

    let conflicting_retry = run(
        &td,
        &[
            "msg",
            "send",
            "--to",
            "bob",
            "--issue",
            &issue,
            "--kind",
            "request",
            "--idempotency-key",
            "request-1",
            "Different body",
            "--actor",
            "alice",
        ],
    );
    assert_eq!(conflicting_retry.status.code(), Some(3));

    let intruder = run(
        &td,
        &["msg", "reply", &request_id, "No", "--actor", "charlie"],
    );
    assert_eq!(intruder.status.code(), Some(3));

    run_ok(
        &td,
        &[
            "msg",
            "reply",
            &request_id,
            "--kind",
            "decline",
            "Cannot take it",
            "--actor",
            "bob",
        ],
    );
    let state = reducer::replay_store(&Store::open(&td.path().join(".mote")).unwrap()).unwrap();
    assert_eq!(
        state.messages[&request_id].request_state,
        Some(RequestState::Declined)
    );
    let events = run_ok(&td, &["--json", "events", "--kind", "message"]);
    let declined: Value = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["type"] == "message.declined")
        .expect("decline event");
    assert_eq!(declined["data"]["reply_to"], request_id);
    assert_eq!(declined["data"]["request_state"], "declined");

    let second_reply = run(
        &td,
        &["msg", "reply", &request_id, "Too late", "--actor", "bob"],
    );
    assert_eq!(second_reply.status.code(), Some(3));
}

#[test]
fn legacy_request_op_without_metadata_replays_as_open_request() {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    let msg_id = "msg-legacy-request".to_string();
    let op = make_msg_send(
        "alice".into(),
        msg_id.clone(),
        "bob".into(),
        None,
        None,
        "request".into(),
        "old op shape".into(),
        Timestamp::now(),
    );
    let json = serde_json::to_value(&op).unwrap();
    assert!(json.get("reply_to").is_none());
    assert!(json.get("correlation_id").is_none());
    assert!(json.get("idempotency_key").is_none());

    publish::publish_op(&store, &op).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let request = &state.messages[&msg_id];
    assert_eq!(request.request_state, Some(RequestState::Open));
    assert_eq!(request.correlation_id.as_deref(), Some(msg_id.as_str()));
}
