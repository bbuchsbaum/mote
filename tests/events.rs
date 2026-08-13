use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

static FOLLOW_PROCESS_LOCK: Mutex<()> = Mutex::new(());

fn init_store(td: &TempDir) {
    let out = Command::new(mote_bin())
        .arg("init")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {:?}", out.stderr);
}

fn new_bead(td: &TempDir, actor: &str) -> String {
    let out = Command::new(mote_bin())
        .args(["new", "event test", "--actor", actor])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "new failed: {:?}", out.stderr);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn send_message(td: &TempDir, from: &str, to: &str, issue: &str, kind: &str, body: &str) {
    let out = Command::new(mote_bin())
        .args([
            "msg", "send", "--to", to, "--issue", issue, "--kind", kind, body, "--actor", from,
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "send failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn high_water_cursor(td: &TempDir) -> String {
    let mut names: Vec<String> = std::fs::read_dir(td.path().join(".mote/ops"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".json"))
        .collect();
    names.sort();
    names.last().unwrap().trim_end_matches(".json").to_string()
}

fn read_follow_line(child: &mut Child, timeout: Duration) -> String {
    let stdout = child.stdout.take().expect("follow stdout must be piped");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        let result = reader.read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(line)) if !line.is_empty() => line,
        Ok(Ok(_)) => panic!("follow stream closed before an event arrived"),
        Ok(Err(e)) => panic!("failed reading follow stream: {e}"),
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("timed out waiting for follow event; stderr: {stderr}")
        }
    }
}

#[test]
fn json_event_schema_contains_full_message_and_filters_actor() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");
    send_message(&td, "alice", "bob", &issue, "request", "take tests");

    let out = Command::new(mote_bin())
        .args([
            "--json",
            "events",
            "--kind",
            "message",
            "--for-actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let line = String::from_utf8(out.stdout).unwrap();
    let event: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(event["schema"], "mote.event.v1");
    assert_eq!(event["type"], "message.sent");
    assert_eq!(event["category"], "message");
    assert_eq!(event["actor"], "alice");
    assert_eq!(event["accepted"], true);
    assert_eq!(event["data"]["to"], "bob");
    assert_eq!(event["data"]["entity"], issue);
    assert_eq!(event["data"]["msg_kind"], "request");
    assert_eq!(event["data"]["body"], "take tests");
    for key in ["event_id", "store_id", "op_id", "ts"] {
        assert!(event[key].is_string(), "missing string key {key}: {event}");
    }

    let unrelated = Command::new(mote_bin())
        .args([
            "--json",
            "events",
            "--kind",
            "message",
            "--for-actor",
            "charlie",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unrelated.status.success());
    assert!(unrelated.stdout.is_empty());
}

#[test]
fn event_cursor_replays_only_later_events() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");
    send_message(&td, "alice", "bob", &issue, "request", "first");
    let cursor = high_water_cursor(&td);
    send_message(&td, "alice", "bob", &issue, "request", "second");

    let out = Command::new(mote_bin())
        .args(["--json", "events", "--kind", "message", "--after", &cursor])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1);
    let event: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(event["data"]["body"], "second");
}

#[test]
fn follow_delivers_a_new_message_before_the_fallback_deadline() {
    let _follow_guard = FOLLOW_PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");
    let cursor = high_water_cursor(&td);

    let mut child = Command::new(mote_bin())
        .args([
            "--json",
            "events",
            "--kind",
            "message",
            "--for-actor",
            "bob",
            "--after",
            &cursor,
            "--follow",
            "--interval",
            "1",
        ])
        .current_dir(td.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Measure delivery after the follower has had a short scheduling window;
    // cursor catch-up still makes publication during startup lossless.
    thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    send_message(&td, "alice", "bob", &issue, "request", "wake up");
    let line = read_follow_line(&mut child, Duration::from_secs(3));
    let elapsed = started.elapsed();
    let event: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(event["type"], "message.sent");
    assert_eq!(event["data"]["body"], "wake up");
    assert!(
        elapsed < Duration::from_secs(3),
        "delivery took {elapsed:?}"
    );

    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn inbox_follow_starts_with_filtered_unacked_messages() {
    let _follow_guard = FOLLOW_PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");
    send_message(&td, "alice", "bob", &issue, "fyi", "ignore me");
    send_message(&td, "alice", "bob", &issue, "request", "take tests");

    let mut child = Command::new(mote_bin())
        .args([
            "--json", "inbox", "--actor", "bob", "--kind", "request", "--follow",
        ])
        .current_dir(td.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let line = read_follow_line(&mut child, Duration::from_secs(2));
    let event: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(event["schema"], "mote.event.v1");
    assert_eq!(event["type"], "message.sent");
    assert_eq!(event["data"]["msg_kind"], "request");
    assert_eq!(event["data"]["body"], "take tests");

    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn human_inbox_follow_uses_compact_message_output() {
    let _follow_guard = FOLLOW_PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");
    send_message(&td, "alice", "bob", &issue, "request", "take tests");

    let mut child = Command::new(mote_bin())
        .args(["inbox", "--actor", "bob", "--follow"])
        .current_dir(td.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let line = read_follow_line(&mut child, Duration::from_secs(2));
    assert!(line.starts_with("[request] alice -> bob  msg=msg-"));
    assert!(line.contains(&format!("issue={issue}")));
    assert!(line.ends_with("take tests\n"));
    assert!(!line.contains("mote.event.v1"));

    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn inbox_wait_returns_after_a_new_delivery() {
    let _follow_guard = FOLLOW_PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");

    let child = Command::new(mote_bin())
        .args([
            "--json",
            "inbox",
            "--actor",
            "bob",
            "--wait",
            "--timeout",
            "5",
            "--interval",
            "1",
        ])
        .current_dir(td.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_millis(100));
    send_message(&td, "alice", "bob", &issue, "request", "wake once");
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "wait failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let messages: Value = serde_json::from_slice(&out.stdout).unwrap();
    let messages = messages.as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["body"], "wake once");
    assert_eq!(messages[0]["to"], "bob");
}

#[test]
fn inbox_wait_returns_an_existing_pending_message_immediately() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "alice");
    send_message(&td, "alice", "bob", &issue, "request", "already here");

    let started = Instant::now();
    let out = Command::new(mote_bin())
        .args([
            "--json",
            "inbox",
            "--actor",
            "bob",
            "--wait",
            "--timeout",
            "5",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "pending inbox should not wait for the timeout"
    );
    let messages: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(messages[0]["body"], "already here");
}

#[test]
fn inbox_wait_timeout_zero_returns_an_empty_inbox() {
    let td = TempDir::new().unwrap();
    init_store(&td);

    let out = Command::new(mote_bin())
        .args([
            "--json",
            "inbox",
            "--actor",
            "bob",
            "--wait",
            "--timeout",
            "0",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let messages: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(messages, serde_json::json!([]));
}
