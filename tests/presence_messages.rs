//! Presence-aware direct delivery remains durable mail, with an opt-in live guard.

use std::fs;
use std::process::{Command, Output};

use jiff::Timestamp;
use serde_json::Value;
use tempfile::TempDir;

use mote::op::{make_msg_send_with_options, make_session_start};
use mote::{publish, reducer, repo::Store};

fn at(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn put(store: &Store, op: &mote::op::Op) -> (String, bool) {
    let name = publish::publish_op(store, op).unwrap();
    let state = reducer::replay_store(store).unwrap();
    let accepted = state.was_accepted(name.as_str());
    (name.into_string(), accepted)
}

fn send_op(
    from: &str,
    id: &str,
    to: &str,
    require_live: bool,
    key: Option<&str>,
    ts: &str,
) -> mote::op::Op {
    make_msg_send_with_options(
        from.into(),
        id.into(),
        to.into(),
        None,
        None,
        "request".into(),
        "please report".into(),
        None,
        None,
        key.map(str::to_string),
        Vec::new(),
        require_live,
        at(ts),
    )
}

#[test]
fn reducer_queues_offline_mail_and_checks_require_live_at_the_exact_boundary() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();

    let (_, accepted) = put(
        &store,
        &send_op(
            "alice",
            "msg-offline",
            "unseen",
            false,
            None,
            "2030-01-01T00:00:00Z",
        ),
    );
    assert!(
        accepted,
        "ordinary direct mail must queue for unseen actors"
    );

    let (_, accepted) = put(
        &store,
        &make_session_start(
            "bob".into(),
            "sess-bob".into(),
            60,
            None,
            None,
            at("2030-01-01T00:01:00Z"),
        ),
    );
    assert!(accepted);
    let (_, accepted) = put(
        &store,
        &send_op(
            "alice",
            "msg-live",
            "bob",
            true,
            Some("live-1"),
            "2030-01-01T00:01:59Z",
        ),
    );
    assert!(accepted);

    let (boundary_op, accepted) = put(
        &store,
        &send_op(
            "alice",
            "msg-boundary",
            "bob",
            true,
            None,
            "2030-01-01T00:02:00Z",
        ),
    );
    assert!(!accepted, "the lease is expired at its exact deadline");
    let (absent_op, accepted) = put(
        &store,
        &send_op(
            "alice",
            "msg-absent",
            "charlie",
            true,
            None,
            "2030-01-01T00:02:01Z",
        ),
    );
    assert!(!accepted);

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.messages.len(), 2);
    let offline = state.messages.get("msg-offline").unwrap();
    assert_eq!(offline.recipient_presence.state, "untracked");
    assert_eq!(offline.recipient_presence.source, "none");
    assert_eq!(state.inbox_for("unseen").len(), 1);
    assert!(offline.ack_ts.is_none());
    assert_eq!(offline.request_state.unwrap().as_str(), "open");
    assert!(offline.response_msg_id.is_none());

    let live = state.messages.get("msg-live").unwrap();
    assert!(live.require_live);
    assert_eq!(live.recipient_presence.state, "live");
    assert_eq!(live.recipient_presence.source, "session_lease");
    assert!(
        state
            .rejection_reason(&boundary_op)
            .unwrap()
            .contains("state=expired source=session_history reason=ttl_elapsed")
    );
    assert!(
        state
            .rejection_reason(&absent_op)
            .unwrap()
            .contains("state=untracked source=none reason=no_presence_evidence")
    );
}

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_cli(temp: &TempDir) {
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

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

fn op_count(temp: &TempDir) -> usize {
    fs::read_dir(temp.path().join(".mote/ops"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

#[test]
fn cli_reports_delivery_evidence_and_preserves_idempotent_retries() {
    let temp = TempDir::new().unwrap();
    init_cli(&temp);
    success(
        &temp,
        "alice",
        &["session", "start", "--as", "bob", "--ttl", "5m"],
    );

    let args = [
        "--json",
        "msg",
        "send",
        "--to",
        "bob",
        "--kind",
        "request",
        "--require-live",
        "--idempotency-key",
        "live-request-1",
        "status?",
    ];
    let first = json_stdout(&success(&temp, "alice", &args));
    assert_eq!(first["accepted"], true);
    assert_eq!(first["delivery"], "queued");
    assert_eq!(first["addressed"], true);
    assert_eq!(first["private"], false);
    assert_eq!(first["require_live"], true);
    assert_eq!(first["recipient_presence"]["state"], "live");
    assert_eq!(first["recipient_presence"]["source"], "session_lease");
    assert_eq!(first["idempotent_retry"], false);
    let after_first = op_count(&temp);

    let retry = json_stdout(&success(&temp, "alice", &args));
    assert_eq!(retry["msg_id"], first["msg_id"]);
    assert_eq!(retry["recipient_presence"], first["recipient_presence"]);
    assert_eq!(retry["idempotent_retry"], true);
    assert_eq!(op_count(&temp), after_first);

    let requests = json_stdout(&success(
        &temp,
        "alice",
        &["--json", "msg", "requests", "--state", "open"],
    ));
    assert_eq!(requests[0]["recipient_presence"]["state"], "live");
    assert_eq!(requests[0]["require_live"], true);
    assert!(requests[0]["ack_ts"].is_null());
    assert!(requests[0]["response_msg_id"].is_null());

    let human = success(
        &temp,
        "alice",
        &["msg", "send", "--to", "unseen", "offline mail"],
    );
    let human_id = String::from_utf8(human.stdout).unwrap();
    assert!(human_id.trim().starts_with("msg-"));
    let diagnostic = String::from_utf8(human.stderr).unwrap();
    assert!(
        diagnostic
            .contains("recipient unseen: untracked source=none reason=no_presence_evidence as-of=")
    );
    assert!(diagnostic.contains("delivery=queued"));
    assert!(
        diagnostic.contains(
            "public fallback: mote discuss post --topic <topic> --notify unseen --body -"
        )
    );

    let rejected = output(
        &temp,
        "alice",
        &[
            "--json",
            "msg",
            "send",
            "--to",
            "charlie",
            "--require-live",
            "should fail",
        ],
    );
    assert_eq!(rejected.status.code(), Some(2));
    let rejected = json_stdout(&rejected);
    assert_eq!(rejected["accepted"], false);
    assert_eq!(rejected["delivery"], "rejected");
    assert_eq!(rejected["recipient_presence"]["state"], "untracked");
    assert_eq!(rejected["recipient_presence"]["source"], "none");

    let events = success(&temp, "alice", &["--json", "events", "--kind", "message"]);
    let events = String::from_utf8(events.stdout).unwrap();
    let accepted_event = events
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["data"]["msg_id"] == first["msg_id"])
        .unwrap();
    assert_eq!(accepted_event["data"]["delivery"], "queued");
    assert_eq!(
        accepted_event["data"]["recipient_presence"]["state"],
        "live"
    );
    assert_eq!(accepted_event["data"]["private"], false);
}
