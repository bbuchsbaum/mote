//! Cross-surface identity invariants for concurrent sessions and direct mail.

use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn command(td: &TempDir, args: &[&str]) -> Command {
    let mut command = Command::new(mote_bin());
    command
        .args(args)
        .current_dir(td.path())
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_SESSION")
        .env_remove("MOTE_STORE");
    command
}

fn output(td: &TempDir, args: &[&str]) -> Output {
    command(td, args).output().unwrap()
}

fn run(td: &TempDir, args: &[&str]) -> String {
    let output = output(td, args);
    assert!(
        output.status.success(),
        "`mote {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run_as(td: &TempDir, actor: &str, args: &[&str]) -> String {
    let mut all = args.to_vec();
    all.extend(["--actor", actor]);
    run(td, &all)
}

fn init(td: &TempDir) {
    run(td, &["init"]);
}

fn set_local_actor(td: &TempDir, actor: &str) {
    run(td, &["actor", "set", actor]);
}

fn start_session(td: &TempDir, actor: &str) -> String {
    let stdout = run(td, &["session", "start", "--as", actor, "--ttl", "1h"]);
    let prefix = "export MOTE_SESSION='";
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|value| value.strip_suffix('\''))
        .unwrap_or_else(|| panic!("missing MOTE_SESSION activation in:\n{stdout}"))
        .to_string()
}

fn json(td: &TempDir, args: &[&str]) -> Value {
    serde_json::from_str(&run(td, args)).unwrap()
}

#[test]
fn local_identity_fails_closed_for_writes_when_sessions_are_concurrent() {
    let td = TempDir::new().unwrap();
    init(&td);
    set_local_actor(&td, "checkout-default");
    assert!(
        output(&td, &["new", "single-user compatibility"])
            .status
            .success()
    );
    start_session(&td, "agent-a");
    start_session(&td, "agent-b");

    let rejected = output(&td, &["new", "must not be misattributed"]);
    assert_eq!(rejected.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert!(stderr.contains("source=local"), "stderr:\n{stderr}");
    assert!(stderr.contains("2 live sessions"), "stderr:\n{stderr}");
    assert!(stderr.contains("export MOTE_ACTOR"), "stderr:\n{stderr}");
    assert!(stderr.contains("session start --as"), "stderr:\n{stderr}");

    let doctor = json(&td, &["--json", "doctor"]);
    let warnings = doctor["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| {
            let warning = warning.as_str().unwrap();
            warning.contains("actor-attributed writes fail closed")
                && warning.contains("session start --as")
                && warning.contains("export MOTE_ACTOR")
        }),
        "doctor warnings are not actionable: {warnings:?}"
    );

    // Diagnosis and recovery stay available under the same ambiguity.
    assert!(output(&td, &["actor", "list"]).status.success());
    assert!(output(&td, &["inbox"]).status.success());
    assert!(
        output(&td, &["session", "start", "--as", "agent-c"])
            .status
            .success()
    );

    // An invocation-scoped identity is sufficient; no confirmation state is
    // stored or inferred from the shared file.
    assert!(
        output(&td, &["new", "explicitly attributed", "--actor", "agent-a"])
            .status
            .success()
    );
}

#[test]
fn changing_local_actor_cannot_retag_an_activated_session() {
    let td = TempDir::new().unwrap();
    init(&td);
    set_local_actor(&td, "agent-a");
    let session_a = start_session(&td, "agent-a");
    start_session(&td, "agent-b");
    set_local_actor(&td, "wrong-shared-name");

    let sent = command(&td, &["msg", "send", "--to", "recipient", "session-owned"])
        .env("MOTE_ACTOR", "agent-a")
        .env("MOTE_SESSION", &session_a)
        .output()
        .unwrap();
    assert!(
        sent.status.success(),
        "explicit session write failed: {}",
        String::from_utf8_lossy(&sent.stderr)
    );

    let inbox = json(&td, &["--json", "--actor", "recipient", "inbox"]);
    assert_eq!(inbox.as_array().unwrap().len(), 1);
    assert_eq!(inbox[0]["from"], "agent-a");

    let ambiguous = output(&td, &["msg", "send", "--to", "recipient", "wrong byline"]);
    assert_eq!(ambiguous.status.code(), Some(3));
    let inbox = json(&td, &["--json", "--actor", "recipient", "inbox"]);
    assert_eq!(inbox.as_array().unwrap().len(), 1);
}

#[test]
fn empty_human_inbox_names_actor_and_source_without_changing_json_shape() {
    let td = TempDir::new().unwrap();
    init(&td);
    set_local_actor(&td, "alice");

    let human = run(&td, &["inbox"]);
    assert_eq!(
        human,
        "inbox for alice (source=local): no unacknowledged messages"
    );

    let explicit = run(&td, &["--actor", "bob", "inbox"]);
    assert_eq!(
        explicit,
        "inbox for bob (source=flag): no unacknowledged messages"
    );

    let inbox_json = json(&td, &["--json", "inbox"]);
    assert_eq!(inbox_json, Value::Array(Vec::new()));
    let identity_json = json(&td, &["--json", "actor", "show"]);
    assert_eq!(identity_json["actor"], "alice");
    assert_eq!(identity_json["source"], "local");
}

fn assert_inbox_count_invariant(td: &TempDir, actor: &str) {
    let actors = json(td, &["--json", "--actor", actor, "actor", "list"]);
    let summary = actors
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["actor"] == actor)
        .unwrap_or_else(|| panic!("actor `{actor}` missing from {actors}"));
    let inbox = json(td, &["--json", "--actor", actor, "inbox"]);
    assert_eq!(
        summary["inbox_unacked"].as_u64().unwrap() as usize,
        inbox.as_array().unwrap().len(),
        "actor-list and explicit inbox diverged for {actor}"
    );
}

fn assert_mail_invariants(td: &TempDir) {
    assert_inbox_count_invariant(td, "alice");
    assert_inbox_count_invariant(td, "bob");
}

#[test]
fn actor_list_inbox_count_invariant_survives_mixed_send_ack_and_reply() {
    let td = TempDir::new().unwrap();
    init(&td);

    let request = run_as(
        &td,
        "alice",
        &[
            "msg",
            "send",
            "--to",
            "bob",
            "--kind",
            "request",
            "please review",
        ],
    );
    assert_mail_invariants(&td);

    let fyi = run_as(
        &td,
        "alice",
        &["msg", "send", "--to", "bob", "--kind", "fyi", "context"],
    );
    assert_mail_invariants(&td);

    let response = run_as(&td, "bob", &["msg", "reply", &request, "review complete"]);
    assert_mail_invariants(&td);

    run_as(&td, "bob", &["msg", "ack", &request]);
    assert_mail_invariants(&td);
    run_as(&td, "bob", &["msg", "ack", &fyi]);
    assert_mail_invariants(&td);
    run_as(&td, "alice", &["msg", "ack", &response]);
    assert_mail_invariants(&td);
}
