use std::process::{Command, Output};
use std::sync::{Arc, Barrier};
use std::thread;

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
    assert!(json.get("answers").is_none());

    publish::publish_op(&store, &op).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let request = &state.messages[&msg_id];
    assert_eq!(request.request_state, Some(RequestState::Open));
    assert_eq!(request.correlation_id.as_deref(), Some(msg_id.as_str()));
}

#[test]
fn one_message_atomically_answers_multiple_requests_with_provenance() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_issue(&td);
    let request_one = run_ok(
        &td,
        &[
            "msg", "send", "--to", "bob", "--issue", &issue, "--kind", "request", "one", "--actor",
            "alice",
        ],
    );
    let request_two = run_ok(
        &td,
        &[
            "msg", "send", "--to", "bob", "--issue", &issue, "--kind", "request", "two", "--actor",
            "alice",
        ],
    );
    let answer_args = [
        "msg",
        "send",
        "--to",
        "alice",
        "--answers",
        &request_one,
        "--answers",
        &request_two,
        "--idempotency-key",
        "answer-both",
        "both done",
        "--actor",
        "bob",
    ];
    let answer_id = run_ok(&td, &answer_args);
    assert_eq!(run_ok(&td, &answer_args), answer_id);
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    for request_id in [&request_one, &request_two] {
        let request = &state.messages[request_id];
        assert_eq!(request.request_state, Some(RequestState::Responded));
        assert_eq!(request.response_msg_id.as_deref(), Some(answer_id.as_str()));
        assert!(request.response_post_id.is_none());
    }
    let mut expected_answers = vec![request_one.clone(), request_two.clone()];
    expected_answers.sort();
    assert_eq!(state.messages[&answer_id].answers, expected_answers);

    let rows: Value = serde_json::from_str(&run_ok(
        &td,
        &[
            "--json",
            "--actor",
            "bob",
            "msg",
            "requests",
            "--state",
            "responded",
        ],
    ))
    .unwrap();
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .all(|row| row["response_msg_id"] == answer_id)
    );
    let events: Vec<Value> = run_ok(&td, &["--json", "events", "--kind", "message"])
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let answer_event = events
        .iter()
        .find(|event| event["data"]["msg_id"] == answer_id)
        .unwrap();
    assert_eq!(answer_event["type"], "message.responded");
    assert_eq!(answer_event["data"]["request_state"], "responded");

    let ordinary = run_ok(
        &td,
        &[
            "msg", "send", "--to", "bob", "--kind", "request", "three", "--actor", "alice",
        ],
    );
    run_ok(
        &td,
        &[
            "msg",
            "send",
            "--to",
            "alice",
            "plain prose",
            "--actor",
            "bob",
        ],
    );
    assert_eq!(
        reducer::replay_store(&store).unwrap().messages[&ordinary].request_state,
        Some(RequestState::Open)
    );
    let unauthorized = run(
        &td,
        &[
            "msg",
            "send",
            "--to",
            "alice",
            "--answers",
            &ordinary,
            "not mine",
            "--actor",
            "charlie",
        ],
    );
    assert_eq!(unauthorized.status.code(), Some(2));
}

#[test]
fn answer_targets_are_validated_as_one_atomic_set() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let valid = run_ok(
        &td,
        &[
            "msg", "send", "--to", "bob", "--kind", "request", "valid", "--actor", "alice",
        ],
    );
    let not_bobs = run_ok(
        &td,
        &[
            "msg", "send", "--to", "charlie", "--kind", "request", "invalid", "--actor", "alice",
        ],
    );

    let failed = run(
        &td,
        &[
            "msg",
            "send",
            "--to",
            "alice",
            "--answers",
            &valid,
            "--answers",
            &not_bobs,
            "must not partly apply",
            "--actor",
            "bob",
        ],
    );
    assert_eq!(failed.status.code(), Some(2));

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.messages.len(),
        2,
        "rejected answer must not create a message"
    );
    assert_eq!(
        state.messages[&valid].request_state,
        Some(RequestState::Open)
    );
    assert_eq!(
        state.messages[&not_bobs].request_state,
        Some(RequestState::Open)
    );
}

#[test]
fn discussion_post_answers_request_and_two_process_answer_races_choose_one() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let request = run_ok(
        &td,
        &[
            "msg", "send", "--to", "bob", "--kind", "request", "report", "--actor", "alice",
        ],
    );
    let posted: Value = serde_json::from_str(&run_ok(
        &td,
        &[
            "--json",
            "discuss",
            "post",
            "--topic",
            "status",
            "--answers",
            &request,
            "public answer",
            "--actor",
            "bob",
        ],
    ))
    .unwrap();
    let post_id = posted["post_id"].as_str().unwrap();
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.messages[&request].response_post_id.as_deref(),
        Some(post_id)
    );
    assert_eq!(state.board_posts[post_id].answers, vec![request.clone()]);
    let thread: Value =
        serde_json::from_str(&run_ok(&td, &["--json", "discuss", "thread", post_id])).unwrap();
    assert_eq!(thread[0]["answers"][0], request);

    for iteration in 0..12 {
        let raced = run_ok(
            &td,
            &[
                "msg",
                "send",
                "--to",
                "bob",
                "--kind",
                "request",
                &format!("race {iteration}"),
                "--actor",
                "alice",
            ],
        );
        let barrier = Arc::new(Barrier::new(3));
        let answer_dir = td.path().to_path_buf();
        let answer_request = raced.clone();
        let answer_barrier = Arc::clone(&barrier);
        let answer = thread::spawn(move || {
            answer_barrier.wait();
            Command::new(mote_bin())
                .args([
                    "msg",
                    "send",
                    "--to",
                    "alice",
                    "--answers",
                    &answer_request,
                    "answer",
                    "--actor",
                    "bob",
                ])
                .current_dir(answer_dir)
                .output()
                .unwrap()
        });
        let reply_dir = td.path().to_path_buf();
        let reply_request = raced.clone();
        let reply_barrier = Arc::clone(&barrier);
        let reply = thread::spawn(move || {
            reply_barrier.wait();
            Command::new(mote_bin())
                .args(["msg", "reply", &reply_request, "reply", "--actor", "bob"])
                .current_dir(reply_dir)
                .output()
                .unwrap()
        });
        barrier.wait();
        let answer = answer.join().unwrap();
        let reply = reply.join().unwrap();
        assert!(
            answer.status.success() || reply.status.success(),
            "at least the replay winner must report success"
        );

        let state = reducer::replay_store(&store).unwrap();
        let request = &state.messages[&raced];
        assert_eq!(request.request_state, Some(RequestState::Responded));
        let winner = request.response_msg_id.as_deref().unwrap();
        let accepted_answers: Vec<_> = state
            .messages
            .values()
            .filter(|message| {
                message.reply_to.as_deref() == Some(raced.as_str())
                    || message.answers.iter().any(|answer| answer == &raced)
            })
            .collect();
        assert_eq!(accepted_answers.len(), 1);
        assert_eq!(accepted_answers[0].msg_id, winner);
    }
}
