//! Schema-style stability tests for `--json` output. We don't byte-pin
//! timestamps and ULIDs (those vary), but we pin the shape: required keys,
//! types, and nested structure. Agents will consume this, so the surface
//! must be predictable.

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_store(td: &TempDir) {
    let out = Command::new(mote_bin())
        .arg("init")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed");
}

fn new_bead(td: &TempDir, title: &str, actor: &str, extra: &[&str]) -> String {
    let mut args: Vec<&str> = vec!["new", title, "--actor", actor];
    args.extend_from_slice(extra);
    let out = Command::new(mote_bin())
        .args(&args)
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "new failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn run_json(td: &TempDir, args: &[&str]) -> Value {
    let mut full: Vec<&str> = vec!["--json"];
    full.extend_from_slice(args);
    let out = Command::new(mote_bin())
        .args(&full)
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "command {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("JSON parse failed for {args:?}: {e}\noutput:\n{stdout}"))
}

fn assert_obj_has_str(v: &Value, key: &str) {
    let val = v
        .get(key)
        .unwrap_or_else(|| panic!("missing key `{key}` in {v}"));
    assert!(
        val.is_string(),
        "expected `{key}` to be a string, got {val} in {v}"
    );
}

fn assert_obj_has_array(v: &Value, key: &str) {
    let val = v
        .get(key)
        .unwrap_or_else(|| panic!("missing key `{key}` in {v}"));
    assert!(
        val.is_array(),
        "expected `{key}` to be an array, got {val} in {v}"
    );
}

fn assert_obj_has_int(v: &Value, key: &str) {
    let val = v
        .get(key)
        .unwrap_or_else(|| panic!("missing key `{key}` in {v}"));
    assert!(
        val.is_i64() || val.is_u64(),
        "expected `{key}` to be an integer, got {val} in {v}"
    );
}

#[test]
fn json_show_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "Auth", "alice", &["-p", "1", "--tag", "backend"]);

    let v = run_json(&td, &["show", &id]);
    let o = v.as_object().expect("show JSON must be object");
    for k in &["id", "title", "status", "body"] {
        assert_obj_has_str(&v, k);
    }
    assert_obj_has_int(&v, "priority");
    for k in &[
        "tags",
        "deps",
        "relations",
        "children",
        "dependents",
        "notes",
    ] {
        assert_obj_has_array(&v, k);
    }
    assert_eq!(o["id"], Value::String(id));
    assert_eq!(o["status"], Value::String("open".into()));
    assert_eq!(o["priority"], 1);
    let tags: Vec<&str> = o["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(tags, vec!["backend"]);
    assert!(o.contains_key("clock"));
}

#[test]
fn json_ls_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    new_bead(&td, "A", "alice", &["-p", "1"]);
    new_bead(&td, "B", "alice", &["-p", "2"]);

    let v = run_json(&td, &["ls"]);
    let arr = v.as_array().expect("ls JSON must be an array");
    assert_eq!(arr.len(), 2);
    for entry in arr {
        for k in &["id", "title", "status"] {
            assert_obj_has_str(entry, k);
        }
        assert_obj_has_int(entry, "priority");
        assert_obj_has_array(entry, "tags");
    }
}

#[test]
fn json_ready_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let _ = new_bead(&td, "A", "alice", &[]);

    let v = run_json(&td, &["ready"]);
    let arr = v.as_array().expect("ready JSON must be an array");
    assert!(!arr.is_empty());
    for entry in arr {
        for k in &["id", "title"] {
            assert_obj_has_str(entry, k);
        }
        assert_obj_has_int(entry, "priority");
    }
}

#[test]
fn json_history_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "h", "alice", &[]);

    let v = run_json(&td, &["history", &id]);
    let arr = v.as_array().unwrap();
    assert!(!arr.is_empty());
    for e in arr {
        for k in &["op_id", "kind", "actor", "ts"] {
            assert_obj_has_str(e, k);
        }
        assert!(e.get("accepted").map(|x| x.is_boolean()).unwrap_or(false));
    }
}

#[test]
fn json_inbox_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "issue", "alice", &[]);

    // alice → bob
    let _ = Command::new(mote_bin())
        .args([
            "msg", "send", "--to", "bob", "--issue", &id, "--kind", "request", "hi", "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();

    let v = run_json(&td, &["--actor", "bob", "inbox"]);
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let m = &arr[0];
    for k in &["msg_id", "from", "to", "msg_kind", "body", "sent_ts"] {
        assert_obj_has_str(m, k);
    }
    assert_obj_has_array(m, "answers");
    for key in [
        "response_post_id",
        "ack_ts",
        "resolved_op_id",
        "resolved_ts",
    ] {
        assert!(
            m.get(key).is_some(),
            "shared message projection lacks {key}: {m}"
        );
    }
    assert!(m["ack_ts"].is_null());
    assert!(m["resolved_op_id"].is_null());
    assert!(m["resolved_ts"].is_null());
    assert!(
        m.get("direction").is_none(),
        "direction is thread-view context only"
    );
    assert_eq!(m["from"], Value::String("alice".into()));
    assert_eq!(m["to"], Value::String("bob".into()));
}

#[test]
fn json_actor_list_includes_authors_and_message_recipients() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "issue", "alice", &[]);

    let sent = Command::new(mote_bin())
        .args([
            "msg", "send", "--to", "bob", "--issue", &id, "--kind", "request", "hi", "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(sent.status.success());

    let v = run_json(&td, &["--actor", "bob", "actor", "list"]);
    let actors = v.as_array().expect("actor list JSON must be an array");
    assert_eq!(actors.len(), 2);
    for actor in actors {
        assert_obj_has_str(actor, "actor");
        for key in [
            "active_claims",
            "active_reservations",
            "orphaned_claims",
            "orphaned_reservations",
            "inbox_unacked",
            "incoming_open_requests",
        ] {
            assert_obj_has_int(actor, key);
        }
        assert!(actor["current"].is_boolean());
        assert!(actor.get("last_activity_ts").is_some());
        assert!(actor.get("last_activity_op_id").is_some());
    }

    let alice = actors
        .iter()
        .find(|actor| actor["actor"] == "alice")
        .unwrap();
    assert_eq!(alice["current"], false);
    assert!(alice["last_activity_ts"].is_string());
    let bob = actors.iter().find(|actor| actor["actor"] == "bob").unwrap();
    assert_eq!(bob["current"], true);
    assert_eq!(bob["inbox_unacked"], 1);
    assert_eq!(bob["incoming_open_requests"], 1);
}

#[test]
fn json_preflight_schema_clear_and_conflict() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth", "alice", &[]);

    // Clear case.
    let v_clear = run_json(
        &td,
        &[
            "--actor",
            "alice",
            "preflight",
            "--issue",
            &id,
            "--paths",
            "src/auth/",
        ],
    );
    let o = v_clear.as_object().unwrap();
    for k in &["issue", "actor"] {
        assert_obj_has_str(&v_clear, k);
    }
    assert_obj_has_array(&v_clear, "paths");
    assert_obj_has_array(&v_clear, "conflicts");
    assert_eq!(o["conflicts"].as_array().unwrap().len(), 0);

    // Conflict case: alice reserves, bob preflights overlapping.
    let _ = Command::new(mote_bin())
        .args(["reserve", "src/auth/", "--issue", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let out = Command::new(mote_bin())
        .args([
            "--json",
            "--actor",
            "bob",
            "preflight",
            "--issue",
            &id,
            "--paths",
            "src/auth/token.rs",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    // exit 2 expected; --json still emits a valid object on stdout.
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let conflicts = v["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1);
    let c = &conflicts[0];
    for k in &[
        "new_path",
        "held_path",
        "actor",
        "reservation_id",
        "disposition",
        "conflict_kind",
    ] {
        assert_obj_has_str(c, k);
    }
}

#[test]
fn json_who_has_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth", "alice", &[]);

    let _ = Command::new(mote_bin())
        .args(["reserve", "src/auth/", "--issue", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();

    let v = run_json(&td, &["who-has", "src/auth/token.rs"]);
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    for k in &[
        "path",
        "actor",
        "reservation_id",
        "entity",
        "lease_until_ts",
        "disposition",
    ] {
        assert_obj_has_str(&arr[0], k);
    }
}

#[test]
fn json_board_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let _ = new_bead(&td, "x", "alice", &[]);

    let v = run_json(&td, &["--actor", "alice", "board"]);
    let o = v.as_object().unwrap();
    assert!(o.contains_key("status_counts"));
    assert_obj_has_array(&v, "active_claims");
    assert_obj_has_array(&v, "active_reservations");
    assert_obj_has_array(&v, "orphaned_claims");
    assert_obj_has_array(&v, "orphaned_reservations");
    assert_obj_has_array(&v, "expiring_reservations");
    assert_obj_has_array(&v, "expired_reservations");
    assert_obj_has_int(&v, "inbox_unacked");
}

#[test]
fn json_discussion_forum_surfaces_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);

    let topic = Command::new(mote_bin())
        .args([
            "discuss",
            "topic",
            "new",
            "planning",
            "--title",
            "Planning",
            "--description",
            "Agent planning board",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(topic.status.success());

    let root = Command::new(mote_bin())
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "Root idea",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(root.status.success());
    let root_id = String::from_utf8(root.stdout).unwrap().trim().to_string();

    let reply = Command::new(mote_bin())
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "--reply-to",
            &root_id,
            "Reply idea",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(reply.status.success());
    let reply_id = String::from_utf8(reply.stdout).unwrap().trim().to_string();

    let sticky = Command::new(mote_bin())
        .args(["discuss", "sticky", &reply_id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(sticky.status.success());

    let topics = run_json(&td, &["discuss", "topics"]);
    let topics_arr = topics.as_array().expect("topics JSON must be an array");
    assert_eq!(topics_arr.len(), 1);
    let topic = &topics_arr[0];
    for k in &[
        "topic",
        "title",
        "body",
        "created_by",
        "created_ts",
        "created_op_id",
        "last_activity_ts",
        "last_activity_op_id",
    ] {
        assert_obj_has_str(topic, k);
    }
    for k in &["post_count", "sticky_count"] {
        assert_obj_has_int(topic, k);
    }
    assert!(topic["explicit"].is_boolean());

    let list = run_json(&td, &["discuss", "list", "--topic", "planning"]);
    let list_arr = list.as_array().expect("list JSON must be an array");
    assert_eq!(list_arr.len(), 2);
    let post = &list_arr[0];
    for k in &["post_id", "from", "topic", "body", "sent_ts"] {
        assert_obj_has_str(post, k);
    }
    assert!(post["sticky"].is_boolean());
    assert!(post.get("reply_to").is_some());
    assert!(post.get("sticky_op_id").is_some());
    assert_obj_has_str(post, "disposition");
    assert_obj_has_array(post, "supersedes");
    assert!(post["retracted"].is_boolean());
    for key in [
        "superseded_by",
        "superseded_op_id",
        "retraction_reason",
        "retracted_op_id",
    ] {
        assert!(post.get(key).is_some());
    }

    let thread = run_json(&td, &["discuss", "thread", &root_id]);
    let thread_arr = thread.as_array().expect("thread JSON must be an array");
    assert_eq!(thread_arr.len(), 2);
    assert_obj_has_int(&thread_arr[0], "depth");
    assert_obj_has_int(&thread_arr[1], "depth");
    assert_eq!(thread_arr[0]["depth"], 0);
    assert_eq!(thread_arr[1]["depth"], 1);

    let search = run_json(&td, &["discuss", "search", "idea"]);
    let search_obj = search.as_object().expect("search JSON must be an object");
    assert!(search_obj["topics"].is_array());
    assert!(search_obj["posts"].is_array());
    assert!(
        search_obj["posts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["post_id"] == root_id)
    );

    let legacy_unread = run_json(&td, &["--actor", "carol", "discuss", "unread"]);
    assert_eq!(legacy_unread.as_array().unwrap().len(), 2);
    let unread = run_json(&td, &["--actor", "carol", "discuss", "unread", "--page"]);
    let unread_obj = unread.as_object().expect("unread JSON must be an object");
    assert_eq!(unread_obj["posts"].as_array().unwrap().len(), 2);
    let page = unread_obj["page"].as_object().unwrap();
    for key in [
        "order",
        "window",
        "count",
        "has_older",
        "has_newer",
        "first_post_id",
        "first_op_id",
        "last_post_id",
        "last_op_id",
        "snapshot_last_post_id",
        "snapshot_last_op_id",
        "effective_cursor_op_id",
    ] {
        assert!(page.contains_key(key), "missing page key {key}");
    }
    assert_eq!(page["order"], "chronological");
    assert_eq!(page["window"], "newest");
}

#[test]
fn json_fsck_schema() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    new_bead(&td, "a", "alice", &[]);

    let v = run_json(&td, &["fsck"]);
    let o = v.as_object().unwrap();
    assert_obj_has_int(&v, "ops_checked");
    for k in &["bad_filename", "bad_json", "bad_hash", "bad_op_shape"] {
        assert_obj_has_array(&v, k);
    }
    assert_obj_has_int(&v, "tmp_total");
    assert_obj_has_int(&v, "tmp_cleaned");
    assert!(o["ops_checked"].as_i64().unwrap() >= 1);
}
