//! M4 acceptance: A5 (claim lease expiry) + A6 (msg send/inbox/ack).

use std::process::Command;
use std::thread;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::Value;
use tempfile::TempDir;

use mote::op::{Op, make_board_retract, make_board_supersede};
use mote::{publish, reducer, repo::Store};

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_store(td: &TempDir) -> Store {
    let out = Command::new(mote_bin())
        .arg("init")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Store::open(&td.path().join(".mote")).unwrap()
}

fn new_bead(td: &TempDir, title: &str, actor: &str) -> String {
    let out = Command::new(mote_bin())
        .args(["new", title, "--actor", actor])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn claim_accepts_human_ttl_and_retains_seconds_on_wire() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "human ttl", "alice");
    let out = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "15m", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let state = reducer::replay_store(&store).unwrap();
    let claim_op_id = state.history[&id]
        .iter()
        .find(|entry| entry.kind == "claim" && entry.accepted)
        .unwrap()
        .op_id
        .clone();
    let op: Op = serde_json::from_slice(
        &std::fs::read(store.ops_dir().join(format!("{claim_op_id}.json"))).unwrap(),
    )
    .unwrap();
    let Op::Claim(claim) = op else {
        panic!("expected claim op")
    };
    assert_eq!(claim.ttl_s, 900);
}

#[test]
fn a5_claim_lease_expiry_yields_to_next_actor() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "shared task", "alice");

    // alice claims with ttl=1 second.
    let claim_a = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "1", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        claim_a.status.success(),
        "alice's claim failed: stderr={}",
        String::from_utf8_lossy(&claim_a.stderr)
    );

    // While alice's lease is still live, bob's claim must be rejected (exit 2).
    let bob_now = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        !bob_now.status.success(),
        "bob should be rejected while alice's lease is live"
    );
    assert_eq!(bob_now.status.code(), Some(2));

    // Wait past the lease.
    thread::sleep(Duration::from_secs(2));

    // bob's claim now succeeds.
    let bob_later = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        bob_later.status.success(),
        "bob's claim after lease expiry failed: stderr={}",
        String::from_utf8_lossy(&bob_later.stderr)
    );

    // Replay: claim now belongs to bob.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    let claim = bead.claim.as_ref().expect("bead must have a claim");
    assert_eq!(claim.claimed_by, "bob");
}

#[test]
fn a6_msg_send_inbox_ack_round_trip() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "auth bug", "alice");
    let bin = mote_bin();

    // alice → bob
    let send_out = Command::new(bin)
        .args([
            "msg",
            "send",
            "--to",
            "bob",
            "--issue",
            &issue,
            "--kind",
            "request",
            "Please take tests",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        send_out.status.success(),
        "msg send failed: stderr={}",
        String::from_utf8_lossy(&send_out.stderr)
    );
    let msg_id = String::from_utf8(send_out.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(msg_id.starts_with("msg-"), "expected msg-... got {msg_id}");

    // bob inbox lists the message.
    let inbox1 = Command::new(bin)
        .args(["inbox", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(inbox1.status.success());
    let s1 = String::from_utf8(inbox1.stdout).unwrap();
    assert!(s1.contains(&msg_id), "expected msg_id in bob's inbox: {s1}");
    assert!(s1.contains("Please take tests"));

    // alice's inbox does not list it (she's the sender, not recipient).
    let inbox_alice = Command::new(bin)
        .args(["inbox", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let sa = String::from_utf8(inbox_alice.stdout).unwrap();
    assert!(
        !sa.contains(&msg_id),
        "alice should not see her own send: {sa}"
    );

    // self-ack by alice (the sender) must fail.
    let bad_ack = Command::new(bin)
        .args(["msg", "ack", &msg_id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        !bad_ack.status.success(),
        "self-ack must be rejected; got success"
    );

    // bob acks.
    let ack = Command::new(bin)
        .args(["msg", "ack", &msg_id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        ack.status.success(),
        "bob ack failed: stderr={}",
        String::from_utf8_lossy(&ack.stderr)
    );

    // bob's inbox is now empty for that msg.
    let inbox2 = Command::new(bin)
        .args(["inbox", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s2 = String::from_utf8(inbox2.stdout).unwrap();
    assert!(
        !s2.contains(&msg_id),
        "bob's inbox should not contain acked msg: {s2}"
    );

    // double ack by bob is rejected.
    let dup = Command::new(bin)
        .args(["msg", "ack", &msg_id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(!dup.status.success(), "duplicate ack must fail");

    // Library-side verification: alice's view of the message has ack info.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let rec = state.messages.get(&msg_id).expect("msg must exist");
    assert_eq!(rec.from, "alice");
    assert_eq!(rec.to, "bob");
    assert_eq!(rec.entity.as_deref(), Some(issue.as_str()));
    assert!(rec.ack_op_id.is_some(), "msg must be acked");
    assert!(rec.ack_ts.is_some());

    // Issue history surfaces both the send and the ack as accepted entries.
    let entries = state.history.get(&issue).expect("issue must have history");
    let kinds_accepted: Vec<&str> = entries
        .iter()
        .filter(|e| e.accepted)
        .map(|e| e.kind.as_str())
        .collect();
    assert!(
        kinds_accepted.contains(&"msg_send"),
        "expected msg_send in accepted history: {kinds_accepted:?}"
    );
    assert!(
        kinds_accepted.contains(&"msg_ack"),
        "expected msg_ack in accepted history: {kinds_accepted:?}"
    );
}

#[test]
fn discussion_board_post_list_topics_and_reply() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let first = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "We should split the parser and tests.",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "first post failed: stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_id = String::from_utf8(first.stdout).unwrap().trim().to_string();
    assert!(
        first_id.starts_with("post-"),
        "expected post id, got {first_id}"
    );

    let reply = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "--reply-to",
            &first_id,
            "I can take the tests.",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        reply.status.success(),
        "reply failed: stderr={}",
        String::from_utf8_lossy(&reply.stderr)
    );
    let reply_id = String::from_utf8(reply.stdout).unwrap().trim().to_string();

    let list = Command::new(bin)
        .args(["discuss", "list", "--topic", "planning"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let s = String::from_utf8(list.stdout).unwrap();
    assert!(s.contains(&first_id), "list output:\n{s}");
    assert!(s.contains(&reply_id), "list output:\n{s}");
    assert!(s.contains("reply="), "list output:\n{s}");
    assert!(s.contains("I can take the tests."), "list output:\n{s}");

    let replies = Command::new(bin)
        .args(["discuss", "replies", &first_id])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(replies.status.success());
    let replies_s = String::from_utf8(replies.stdout).unwrap();
    assert!(
        replies_s.contains(&reply_id),
        "replies output:\n{replies_s}"
    );
    assert!(
        !replies_s.lines().any(|line| line.starts_with(&first_id)),
        "replies output should not include the parent as a row:\n{replies_s}"
    );

    let topics = Command::new(bin)
        .args(["discuss", "topics"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(topics.status.success());
    let topics_s = String::from_utf8(topics.stdout).unwrap();
    assert!(
        topics_s.contains("planning") && topics_s.contains("posts=2"),
        "topics output:\n{topics_s}"
    );

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let reply_record = state
        .board_posts
        .get(&reply_id)
        .expect("reply should be recorded");
    assert_eq!(reply_record.reply_to.as_deref(), Some(first_id.as_str()));
    assert_eq!(state.board_posts_for(Some("planning")).len(), 2);
}

#[test]
fn discussion_post_accepts_body_flag_and_rejects_ambiguous_text() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let post = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "--body",
            "Flag-shaped body",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        post.status.success(),
        "--body post failed: stderr={}",
        String::from_utf8_lossy(&post.stderr)
    );
    let post_id = String::from_utf8(post.stdout).unwrap().trim().to_string();

    let list = Command::new(bin)
        .args(["discuss", "list", "--topic", "planning"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_s = String::from_utf8(list.stdout).unwrap();
    assert!(list_s.contains(&post_id), "list output:\n{list_s}");
    assert!(
        list_s.contains("Flag-shaped body"),
        "list output:\n{list_s}"
    );

    let ambiguous = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "--body",
            "Flag body",
            "Positional body",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(ambiguous.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        stderr.contains("either as positional text or --body, not both"),
        "stderr:\n{stderr}"
    );

    let missing = Command::new(bin)
        .args(["discuss", "post", "--topic", "planning", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("discussion post text is required"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn discussion_topic_new_body_creates_visible_initial_post() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let bin = mote_bin();

    let topic = Command::new(bin)
        .args([
            "discuss",
            "topic",
            "new",
            "planning",
            "--title",
            "Planning",
            "--description",
            "Planning metadata",
            "--body",
            "Initial visible proposal",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        topic.status.success(),
        "topic creation failed: stderr={}",
        String::from_utf8_lossy(&topic.stderr)
    );
    assert_eq!(String::from_utf8(topic.stdout).unwrap().trim(), "planning");
    let stderr = String::from_utf8_lossy(&topic.stderr);
    assert!(
        stderr.contains("with initial post post-"),
        "stderr:\n{stderr}"
    );

    let state = reducer::replay_store(&store).unwrap();
    let topic_record = state.board_topics.get("planning").unwrap();
    assert_eq!(topic_record.body, "Planning metadata");
    assert_eq!(topic_record.post_count, 1);
    let posts = state.board_posts_for(Some("planning"));
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0].body, "Initial visible proposal");

    let list = Command::new(bin)
        .args(["discuss", "list", "--topic", "planning"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_s = String::from_utf8(list.stdout).unwrap();
    assert!(
        list_s.contains("Initial visible proposal"),
        "list output:\n{list_s}"
    );
}

#[test]
fn discussion_board_unread_cursor_tracks_new_external_posts() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let first = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "general",
            "Initial note",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(first.status.success());
    let first_id = String::from_utf8(first.stdout).unwrap().trim().to_string();
    assert!(first_id.starts_with("post-"));

    let unread1 = Command::new(bin)
        .args(["discuss", "unread", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread1.status.success());
    let unread1_s = String::from_utf8(unread1.stdout).unwrap();
    assert!(
        unread1_s.contains(&first_id),
        "bob should see alice's post as unread:\n{unread1_s}"
    );

    let marked = Command::new(bin)
        .args(["discuss", "mark-read", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(marked.status.success());
    let marked_s = String::from_utf8(marked.stdout).unwrap();
    assert!(
        marked_s.contains(&first_id),
        "mark-read prints the cursor post identity:\n{marked_s}"
    );

    let unread2 = Command::new(bin)
        .args(["discuss", "unread", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread2.status.success());
    let unread2_s = String::from_utf8(unread2.stdout).unwrap();
    assert!(
        !unread2_s.contains(&first_id),
        "marked post should no longer be unread:\n{unread2_s}"
    );

    let own = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "general",
            "Bob's own note",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(own.status.success());
    let own_id = String::from_utf8(own.stdout).unwrap().trim().to_string();

    let unread3 = Command::new(bin)
        .args(["discuss", "unread", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread3.status.success());
    let unread3_s = String::from_utf8(unread3.stdout).unwrap();
    assert!(
        !unread3_s.contains(&own_id),
        "an actor should not see its own post as unread:\n{unread3_s}"
    );

    let second = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "general",
            "--reply-to",
            &first_id,
            "Follow-up from alice",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(second.status.success());
    let second_id = String::from_utf8(second.stdout).unwrap().trim().to_string();

    let unread4 = Command::new(bin)
        .args(["discuss", "unread", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread4.status.success());
    let unread4_s = String::from_utf8(unread4.stdout).unwrap();
    assert!(
        unread4_s.contains(&second_id),
        "new reply should be unread and addressable by post id:\n{unread4_s}"
    );
}

#[test]
fn discussion_unread_pages_are_chronological_stable_and_markable_through() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let run_ok = |args: &[&str]| {
        let out = Command::new(mote_bin())
            .args(args)
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "command {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let mut post_ids = Vec::new();
    for body in ["one", "two", "three", "four", "five"] {
        post_ids.push(run_ok(&[
            "discuss", "post", "--topic", "general", body, "--actor", "alice",
        ]));
    }
    run_ok(&["discuss", "sticky", &post_ids[0], "--actor", "alice"]);

    let newest: Value = serde_json::from_str(&run_ok(&[
        "--json", "discuss", "unread", "--page", "--limit", "2", "--actor", "bob",
    ]))
    .unwrap();
    let newest_ids: Vec<_> = newest["posts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|post| post["post_id"].as_str().unwrap())
        .collect();
    assert_eq!(newest_ids, vec![post_ids[3].as_str(), post_ids[4].as_str()]);
    assert_eq!(newest["page"]["order"], "chronological");
    assert_eq!(newest["page"]["window"], "newest");
    assert_eq!(newest["page"]["has_older"], true);
    assert_eq!(newest["page"]["snapshot_last_post_id"], post_ids[4]);

    // A concurrent append is newer than the captured first-page boundary and
    // therefore cannot create a duplicate or gap while paging backward.
    let appended = run_ok(&[
        "discuss", "post", "--topic", "general", "six", "--actor", "alice",
    ]);
    let middle: Value = serde_json::from_str(&run_ok(&[
        "--json",
        "discuss",
        "unread",
        "--page",
        "--before",
        &post_ids[3],
        "--limit",
        "2",
        "--actor",
        "bob",
    ]))
    .unwrap();
    let oldest: Value = serde_json::from_str(&run_ok(&[
        "--json",
        "discuss",
        "unread",
        "--page",
        "--before",
        &post_ids[1],
        "--limit",
        "2",
        "--actor",
        "bob",
    ]))
    .unwrap();
    let paged: Vec<_> = oldest["posts"]
        .as_array()
        .unwrap()
        .iter()
        .chain(middle["posts"].as_array().unwrap())
        .chain(newest["posts"].as_array().unwrap())
        .map(|post| post["post_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        paged,
        post_ids[..5].iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(!paged.contains(&appended.as_str()));

    run_ok(&[
        "discuss",
        "mark-read",
        "--through",
        &post_ids[4],
        "--actor",
        "bob",
    ]);
    let remaining: Value = serde_json::from_str(&run_ok(&[
        "--json", "discuss", "unread", "--page", "--actor", "bob",
    ]))
    .unwrap();
    assert_eq!(remaining["posts"].as_array().unwrap().len(), 1);
    assert_eq!(remaining["posts"][0]["post_id"], appended);

    let older = Command::new(mote_bin())
        .args([
            "discuss",
            "mark-read",
            "--through",
            &post_ids[2],
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(older.status.code(), Some(2));
    for args in [
        vec![
            "discuss",
            "mark-read",
            "--through",
            "post-missing",
            "--actor",
            "bob",
        ],
        vec![
            "discuss",
            "mark-read",
            "--topic",
            "other",
            "--through",
            &appended,
            "--actor",
            "bob",
        ],
    ] {
        let out = Command::new(mote_bin())
            .args(args)
            .current_dir(td.path())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3));
    }
}

#[test]
fn discussion_supersession_and_retraction_are_author_owned_and_deterministic() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let post = |topic: &str, body: &str, actor: &str| {
        let out = Command::new(mote_bin())
            .args(["discuss", "post", "--topic", topic, body, "--actor", actor])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let old = post("design", "old instruction", "alice");
    let replacement = post("design", "replacement instruction", "alice");
    let supersede = Command::new(mote_bin())
        .args([
            "discuss",
            "supersede",
            &old,
            &replacement,
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(supersede.status.success());

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.board_posts[&old].disposition(), "superseded");
    assert_eq!(
        state.board_posts[&old].superseded_by.as_deref(),
        Some(replacement.as_str())
    );
    assert_eq!(
        state.board_posts[&replacement].supersedes,
        vec![old.clone()]
    );
    let listed = Command::new(mote_bin())
        .args(["discuss", "list", "--topic", "design"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.contains(&format!("status=superseded-by:{replacement}")));
    assert!(listed.contains(&format!("status=active supersedes={old}")));

    // Reversing the link would form a cycle, and an already-obsolete post
    // cannot win a second competing disposition.
    for args in [
        vec![
            "discuss",
            "supersede",
            &replacement,
            &old,
            "--actor",
            "alice",
        ],
        vec![
            "discuss",
            "supersede",
            &replacement,
            &replacement,
            "--actor",
            "alice",
        ],
    ] {
        let out = Command::new(mote_bin())
            .args(args)
            .current_dir(td.path())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
    }
    let cross_topic = post("other", "other topic", "alice");
    let unauthorized = post("design", "bob's post", "bob");
    for (new, actor) in [(&cross_topic, "alice"), (&unauthorized, "alice")] {
        let active = post("design", "fresh old", "alice");
        let out = Command::new(mote_bin())
            .args(["discuss", "supersede", &active, new, "--actor", actor])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
    }

    let withdrawn = post("design", "withdraw me", "alice");
    let multiline = Command::new(mote_bin())
        .args([
            "discuss",
            "retract",
            &withdrawn,
            "--reason",
            "first line\nsecond line",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(multiline.status.code(), Some(3));
    let denied = Command::new(mote_bin())
        .args([
            "discuss", "retract", &withdrawn, "--reason", "wrong", "--actor", "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(2));
    let retracted = Command::new(mote_bin())
        .args([
            "discuss",
            "retract",
            &withdrawn,
            "--reason",
            "premise disproved",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(retracted.status.success());
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.board_posts[&withdrawn].disposition(), "retracted");
    assert_eq!(
        state.board_posts[&withdrawn].retraction_reason.as_deref(),
        Some("premise disproved")
    );

    let reducer_guard = post("design", "reducer guard", "alice");
    let invalid_op = make_board_retract(
        "alice".into(),
        reducer_guard.clone(),
        "line one\nline two".into(),
        Timestamp::now(),
    );
    let invalid_name = publish::publish_op(&store, &invalid_op).unwrap();
    let guarded = reducer::replay_store(&store).unwrap();
    assert!(!guarded.was_accepted(invalid_name.as_str()));
    assert_eq!(guarded.board_posts[&reducer_guard].disposition(), "active");

    // Two concurrently publishable supersessions reduce to one winner in
    // immutable op-name order, regardless of directory iteration order.
    let raced_old = post("race", "old", "alice");
    let candidate_a = post("race", "candidate a", "alice");
    let candidate_b = post("race", "candidate b", "alice");
    let timestamp = Timestamp::now();
    let op_a = make_board_supersede(
        "alice".into(),
        raced_old.clone(),
        candidate_a.clone(),
        timestamp,
    );
    let op_b = make_board_supersede(
        "alice".into(),
        raced_old.clone(),
        candidate_b.clone(),
        timestamp,
    );
    let name_a = publish::publish_op(&store, &op_a).unwrap();
    let name_b = publish::publish_op(&store, &op_b).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert_ne!(
        state.was_accepted(name_a.as_str()),
        state.was_accepted(name_b.as_str())
    );
    let winner = state.board_posts[&raced_old]
        .superseded_by
        .as_deref()
        .unwrap();
    assert!(winner == candidate_a || winner == candidate_b);
    assert_eq!(
        reducer::replay_store(&store).unwrap().board_posts[&raced_old]
            .superseded_by
            .as_deref(),
        Some(winner)
    );
}

#[test]
fn discussion_topic_creation_search_activity_and_sticky_posts() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let topic = Command::new(bin)
        .args([
            "discuss",
            "topic",
            "new",
            "release",
            "--title",
            "Release coordination",
            "--description",
            "Where agents coordinate release work",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        topic.status.success(),
        "topic creation failed: stderr={}",
        String::from_utf8_lossy(&topic.stderr)
    );
    let topic_out = String::from_utf8(topic.stdout).unwrap();
    assert_eq!(topic_out.trim(), "release");

    let search_topic = Command::new(bin)
        .args(["discuss", "search", "coordination"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(search_topic.status.success());
    let search_topic_s = String::from_utf8(search_topic.stdout).unwrap();
    assert!(
        search_topic_s.contains("topic  release") && search_topic_s.contains("created="),
        "new explicit topic should surface in search:\n{search_topic_s}"
    );

    let post1 = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "release",
            "Beta checklist is the current priority.",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(post1.status.success());
    let post1_id = String::from_utf8(post1.stdout).unwrap().trim().to_string();
    assert!(post1_id.starts_with("post-"));

    let post2 = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "release",
            "Final signoff can wait.",
            "--actor",
            "carol",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(post2.status.success());
    let post2_id = String::from_utf8(post2.stdout).unwrap().trim().to_string();

    let sticky = Command::new(bin)
        .args(["discuss", "sticky", &post2_id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        sticky.status.success(),
        "sticky failed: stderr={}",
        String::from_utf8_lossy(&sticky.stderr)
    );

    let list = Command::new(bin)
        .args(["discuss", "list", "--topic", "release"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_s = String::from_utf8(list.stdout).unwrap();
    let first_line = list_s.lines().next().unwrap_or_default();
    assert!(
        first_line.starts_with(&post2_id) && first_line.contains("sticky"),
        "sticky post should sort first:\n{list_s}"
    );
    assert!(list_s.contains(&post1_id), "list output:\n{list_s}");

    let topics = Command::new(bin)
        .args(["discuss", "topics"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(topics.status.success());
    let topics_s = String::from_utf8(topics.stdout).unwrap();
    let release_line = topics_s
        .lines()
        .find(|line| line.starts_with("release "))
        .expect("release topic line");
    assert!(
        release_line.contains("posts=2")
            && release_line.contains("sticky=1")
            && release_line.contains("created=")
            && release_line.contains("last="),
        "topic should expose activity metadata:\n{release_line}"
    );

    let search_post = Command::new(bin)
        .args(["discuss", "search", "signoff"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(search_post.status.success());
    let search_post_s = String::from_utf8(search_post.stdout).unwrap();
    assert!(
        search_post_s.contains(&post2_id) && search_post_s.contains("sticky"),
        "sticky post identity should surface in search:\n{search_post_s}"
    );

    let unsticky = Command::new(bin)
        .args(["discuss", "unsticky", &post2_id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unsticky.status.success());

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let release = state.board_topics.get("release").expect("release topic");
    assert!(release.explicit);
    assert_eq!(release.post_count, 2);
    assert_eq!(release.sticky_count, 0);
    assert!(!state.board_posts[&post2_id].sticky);
}

#[test]
fn discussion_list_and_search_limit_keep_stickies_with_newest_hits() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let sticky = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "release",
            "matching anchor note",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(sticky.status.success());
    let sticky_id = String::from_utf8(sticky.stdout).unwrap().trim().to_string();

    let sticky_out = Command::new(bin)
        .args(["discuss", "sticky", &sticky_id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(sticky_out.status.success());

    let mut non_sticky_ids = Vec::new();
    for idx in 0..6 {
        let post = Command::new(bin)
            .args([
                "discuss",
                "post",
                "--topic",
                "release",
                &format!("matching update {idx}"),
                "--actor",
                "bob",
            ])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(post.status.success());
        non_sticky_ids.push(String::from_utf8(post.stdout).unwrap().trim().to_string());
    }

    let list = Command::new(bin)
        .args(["discuss", "list", "--topic", "release", "--limit", "5"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_s = String::from_utf8(list.stdout).unwrap();
    assert!(list_s.contains(&sticky_id), "limited list:\n{list_s}");
    assert!(
        !list_s.contains(&non_sticky_ids[0]) && !list_s.contains(&non_sticky_ids[1]),
        "limited list should trim oldest non-sticky posts:\n{list_s}"
    );
    assert!(
        list_s.contains(non_sticky_ids.last().unwrap()),
        "limited list should keep newest non-sticky post:\n{list_s}"
    );

    let search = Command::new(bin)
        .args([
            "discuss", "search", "matching", "--topic", "release", "--limit", "3",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(search.status.success());
    let search_s = String::from_utf8(search.stdout).unwrap();
    assert!(search_s.contains(&sticky_id), "limited search:\n{search_s}");
    assert!(
        search_s.contains(non_sticky_ids.last().unwrap()) && !search_s.contains(&non_sticky_ids[0]),
        "limited search should keep sticky plus newest matching posts:\n{search_s}"
    );
}

#[test]
fn discussion_list_and_search_limit_caps_sticky_posts() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    for idx in 0..4 {
        let post = Command::new(bin)
            .args([
                "discuss",
                "post",
                "--topic",
                "sticky-limit",
                &format!("unique-match sticky note {idx}"),
                "--actor",
                "alice",
            ])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(post.status.success());
        let post_id = String::from_utf8(post.stdout).unwrap().trim().to_string();

        let sticky = Command::new(bin)
            .args(["discuss", "sticky", &post_id, "--actor", "alice"])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(sticky.status.success());
    }

    let list = Command::new(bin)
        .args(["discuss", "list", "--topic", "sticky-limit", "--limit", "3"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    let list_s = String::from_utf8(list.stdout).unwrap();
    let list_lines: Vec<_> = list_s.lines().collect();
    assert_eq!(list_lines.len(), 3, "limited sticky list:\n{list_s}");
    assert!(
        list_lines.iter().all(|line| line.contains(" sticky")),
        "limited sticky list should contain only sticky posts:\n{list_s}"
    );

    let search = Command::new(bin)
        .args([
            "discuss",
            "search",
            "unique-match",
            "--topic",
            "sticky-limit",
            "--limit",
            "2",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(search.status.success());
    let search_s = String::from_utf8(search.stdout).unwrap();
    let post_lines: Vec<_> = search_s
        .lines()
        .filter(|line| line.starts_with("post   "))
        .collect();
    assert_eq!(post_lines.len(), 2, "limited sticky search:\n{search_s}");
    assert!(
        post_lines.iter().all(|line| line.contains(" sticky")),
        "limited sticky search should contain only sticky posts:\n{search_s}"
    );
}

#[test]
fn discussion_thread_view_and_topic_read_cursor_support_forum_workflow() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let root = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ideas",
            "Root proposal for agent collaboration.",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(root.status.success());
    let root_id = String::from_utf8(root.stdout).unwrap().trim().to_string();

    let reply = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ideas",
            "--reply-to",
            &root_id,
            "First response with refinement.",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(reply.status.success());
    let reply_id = String::from_utf8(reply.stdout).unwrap().trim().to_string();

    let nested = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ideas",
            "--reply-to",
            &reply_id,
            "Nested synthesis.",
            "--actor",
            "carol",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(nested.status.success());
    let nested_id = String::from_utf8(nested.stdout).unwrap().trim().to_string();

    let other = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ops",
            "Operational update.",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(other.status.success());
    let other_id = String::from_utf8(other.stdout).unwrap().trim().to_string();

    let thread = Command::new(bin)
        .args(["discuss", "thread", &root_id])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(thread.status.success());
    let thread_s = String::from_utf8(thread.stdout).unwrap();
    let root_pos = thread_s.find(&root_id).expect("root shown");
    let reply_pos = thread_s.find(&reply_id).expect("reply shown");
    let nested_pos = thread_s.find(&nested_id).expect("nested reply shown");
    assert!(
        root_pos < reply_pos && reply_pos < nested_pos,
        "thread should preserve parent-before-child order:\n{thread_s}"
    );
    let nested_line = thread_s
        .lines()
        .find(|line| line.contains(&nested_id))
        .unwrap_or_default();
    assert!(
        nested_line.starts_with("    "),
        "nested replies should be indented:\n{thread_s}"
    );
    assert!(
        !thread_s.contains(&other_id),
        "thread should not include another topic/root:\n{thread_s}"
    );

    let mark = Command::new(bin)
        .args([
            "discuss",
            "mark-read",
            "--topic",
            "ideas",
            "--actor",
            "dana",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(mark.status.success());

    let unread_ideas = Command::new(bin)
        .args(["discuss", "unread", "--topic", "ideas", "--actor", "dana"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread_ideas.status.success());
    let unread_ideas_s = String::from_utf8(unread_ideas.stdout).unwrap();
    assert!(
        !unread_ideas_s.contains(&root_id) && !unread_ideas_s.contains(&nested_id),
        "topic cursor should hide read ideas posts:\n{unread_ideas_s}"
    );

    let unread_ops = Command::new(bin)
        .args(["discuss", "unread", "--topic", "ops", "--actor", "dana"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread_ops.status.success());
    let unread_ops_s = String::from_utf8(unread_ops.stdout).unwrap();
    assert!(
        unread_ops_s.contains(&other_id),
        "topic cursor should not hide other topics:\n{unread_ops_s}"
    );

    let unread_all = Command::new(bin)
        .args(["discuss", "unread", "--actor", "dana"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread_all.status.success());
    let unread_all_s = String::from_utf8(unread_all.stdout).unwrap();
    assert!(
        !unread_all_s.contains(&root_id)
            && !unread_all_s.contains(&reply_id)
            && !unread_all_s.contains(&nested_id),
        "global unread should honor topic-specific cursors:\n{unread_all_s}"
    );
    assert!(
        unread_all_s.contains(&other_id),
        "global unread should still show unread posts from other topics:\n{unread_all_s}"
    );

    let later = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ideas",
            "Later idea after topic-specific mark-read.",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(later.status.success());
    let later_id = String::from_utf8(later.stdout).unwrap().trim().to_string();

    let mark_global = Command::new(bin)
        .args(["discuss", "mark-read", "--actor", "dana"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(mark_global.status.success());

    let unread_ideas_after_global = Command::new(bin)
        .args(["discuss", "unread", "--topic", "ideas", "--actor", "dana"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread_ideas_after_global.status.success());
    let unread_ideas_after_global_s = String::from_utf8(unread_ideas_after_global.stdout).unwrap();
    assert!(
        !unread_ideas_after_global_s.contains(&later_id),
        "global cursor should hide newer ideas posts despite an older topic cursor:\n{unread_ideas_after_global_s}"
    );

    let unread_all_after_global = Command::new(bin)
        .args(["discuss", "unread", "--actor", "dana"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(unread_all_after_global.status.success());
    let unread_all_after_global_s = String::from_utf8(unread_all_after_global.stdout).unwrap();
    assert!(
        !unread_all_after_global_s.contains(&later_id),
        "global unread should use the later cursor for each topic:\n{unread_all_after_global_s}"
    );
}

#[test]
fn discussion_mark_read_empty_topic_reports_no_posts() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let topic = Command::new(bin)
        .args(["discuss", "topic", "new", "empty", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(topic.status.success());

    let mark = Command::new(bin)
        .args(["discuss", "mark-read", "--topic", "empty", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(mark.status.success());
    let stderr = String::from_utf8_lossy(&mark.stderr);
    assert!(
        stderr.contains("no posts in topic empty"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn discussion_topics_upgrade_implicit_once_and_reject_duplicate_explicit_create() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let implicit_post = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "architecture",
            "Initial architecture note.",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(implicit_post.status.success());

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state1 = reducer::replay_store(&store).unwrap();
    let implicit = state1
        .board_topics
        .get("architecture")
        .expect("implicit topic");
    assert!(!implicit.explicit);
    assert_eq!(implicit.post_count, 1);
    let implicit_created_by = implicit.created_by.clone();
    let implicit_created_ts = implicit.created_ts.clone();
    let implicit_created_op_id = implicit.created_op_id.clone();

    let upgrade = Command::new(bin)
        .args([
            "discuss",
            "topic",
            "new",
            "architecture",
            "--title",
            "Architecture strategy",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        upgrade.status.success(),
        "implicit topic upgrade failed: stderr={}",
        String::from_utf8_lossy(&upgrade.stderr)
    );

    let duplicate = Command::new(bin)
        .args([
            "discuss",
            "topic",
            "new",
            "architecture",
            "--title",
            "Duplicate",
            "--actor",
            "carol",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(duplicate.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(
        stderr.contains("discussion topic architecture already exists"),
        "stderr:\n{stderr}"
    );

    let state2 = reducer::replay_store(&store).unwrap();
    let upgraded = state2
        .board_topics
        .get("architecture")
        .expect("upgraded topic");
    assert!(upgraded.explicit);
    assert_eq!(upgraded.title, "Architecture strategy");
    assert_eq!(upgraded.post_count, 1);
    assert_eq!(upgraded.created_by, implicit_created_by);
    assert_eq!(upgraded.created_ts, implicit_created_ts);
    assert_eq!(upgraded.created_op_id, implicit_created_op_id);
}

#[test]
fn discussion_sticky_is_idempotent_and_rejects_unknown_posts() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let post = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "triage",
            "Important triage note.",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(post.status.success());
    let post_id = String::from_utf8(post.stdout).unwrap().trim().to_string();

    let sticky = Command::new(bin)
        .args(["discuss", "sticky", &post_id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        sticky.status.success(),
        "sticky failed: stderr={}",
        String::from_utf8_lossy(&sticky.stderr)
    );

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state_after_first = reducer::replay_store(&store).unwrap();
    let first_activity = state_after_first.board_topics["triage"]
        .last_activity_op_id
        .clone();

    let sticky_again = Command::new(bin)
        .args(["discuss", "sticky", &post_id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        sticky_again.status.success(),
        "sticky should be idempotent: stderr={}",
        String::from_utf8_lossy(&sticky_again.stderr)
    );

    let state = reducer::replay_store(&store).unwrap();
    assert!(state.board_posts[&post_id].sticky);
    assert_eq!(state.board_topics["triage"].sticky_count, 1);
    assert_eq!(
        state.board_topics["triage"].last_activity_op_id,
        first_activity
    );

    let missing = Command::new(bin)
        .args(["discuss", "sticky", "post-missing", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(
        stderr.contains("no such board post post-missing"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn discussion_search_topic_filter_keeps_results_in_scope() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let design = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "design",
            "shared parser idea",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(design.status.success());
    let design_id = String::from_utf8(design.stdout).unwrap().trim().to_string();

    let ops = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ops",
            "shared deployment idea",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ops.status.success());
    let ops_id = String::from_utf8(ops.stdout).unwrap().trim().to_string();

    let filtered = Command::new(bin)
        .args(["discuss", "search", "shared", "--topic", "design"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(filtered.status.success());
    let filtered_s = String::from_utf8(filtered.stdout).unwrap();
    assert!(
        filtered_s.contains(&design_id),
        "search output:\n{filtered_s}"
    );
    assert!(
        !filtered_s.contains(&ops_id),
        "topic-filtered search leaked another topic:\n{filtered_s}"
    );
}

#[test]
fn discussion_board_rejects_missing_reply_parent() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let out = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "--reply-to",
            "post-missing",
            "Reply to nowhere",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("reply_to post post-missing does not exist"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn discussion_board_rejects_cross_topic_reply() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let parent = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "planning",
            "Planning root",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(parent.status.success());
    let parent_id = String::from_utf8(parent.stdout).unwrap().trim().to_string();

    let out = Command::new(bin)
        .args([
            "discuss",
            "post",
            "--topic",
            "ops",
            "--reply-to",
            &parent_id,
            "Wrong topic reply",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        stderr.contains("is in topic planning, not ops"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn claim_release_round_trip_via_cli() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "task", "alice");

    let claim = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(claim.status.success());

    // Same actor re-claim should succeed (auto-fills expect_claim).
    let renew = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "60", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        renew.status.success(),
        "alice should be able to renew her own claim"
    );

    // Different actor's claim is rejected while lease is live.
    let foreign = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(!foreign.status.success());
    assert_eq!(foreign.status.code(), Some(2));

    // alice releases.
    let release = Command::new(mote_bin())
        .args(["release", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        release.status.success(),
        "release failed: stderr={}",
        String::from_utf8_lossy(&release.stderr)
    );

    // Now bob can claim.
    let bob_claim = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(bob_claim.status.success());

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let claim_state = state.beads[&id].claim.as_ref().expect("must have claim");
    assert_eq!(claim_state.claimed_by, "bob");
}

#[test]
fn ready_excludes_foreign_claimed() {
    use jiff::Timestamp;
    use mote::ids;

    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "ready test", "alice");

    // Alice claims it.
    let _ = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "60", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());

    // is_ready alone says yes (status=open, no deps).
    assert!(state.is_ready(&state.beads[&id]));

    // ready_beads_for("bob", now) excludes the bead.
    let bob_ready: Vec<&str> = state
        .ready_beads_for("bob", &now)
        .map(|b| b.id.as_str())
        .collect();
    assert!(
        !bob_ready.contains(&id.as_str()),
        "bob should not see foreign-claimed bead"
    );

    // ready_beads_for("alice", now) still includes it (her own claim).
    let alice_ready: Vec<&str> = state
        .ready_beads_for("alice", &now)
        .map(|b| b.id.as_str())
        .collect();
    assert!(alice_ready.contains(&id.as_str()));
}

/// The gap the console exposed: `inbox` is inbound-and-unacked-only and
/// `msg requests` covers request roots, so a plain `note`/`fyi` the actor sent
/// was listable nowhere. `msg thread` folds both directions.
#[test]
fn msg_thread_shows_both_directions_including_sent_and_acked() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "parser panic", "alice");
    let bin = mote_bin();

    let send = |from: &str, to: &str, kind: &str, body: &str, issue: Option<&str>| -> String {
        let mut args = vec![
            "msg", "send", "--to", to, "--kind", kind, body, "--actor", from,
        ];
        if let Some(i) = issue {
            args.extend_from_slice(&["--issue", i]);
        }
        let out = Command::new(bin)
            .args(&args)
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "send failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let request = send(
        "bob",
        "alice",
        "request",
        "please take the parser work",
        Some(&issue),
    );
    let sent_fyi = send(
        "alice",
        "bob",
        "fyi",
        "taking it, reserving src/parser.rs",
        None,
    );
    let reply = send("bob", "alice", "note", "thanks, tests behind you", None);
    let unrelated = send("alice", "carol", "note", "unrelated third party", None);

    // alice acks the request, which removes it from her inbox entirely.
    let ack = Command::new(bin)
        .args(["msg", "ack", &request, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ack.status.success());

    // Neither existing surface can see the message alice sent.
    let inbox = Command::new(bin)
        .args(["inbox", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let inbox_s = String::from_utf8(inbox.stdout).unwrap();
    assert!(
        !inbox_s.contains(&sent_fyi),
        "inbox must not list a message alice sent: {inbox_s}"
    );
    assert!(
        !inbox_s.contains(&request),
        "inbox must not list an acked message: {inbox_s}"
    );

    let requests = Command::new(bin)
        .args(["msg", "requests", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let requests_s = String::from_utf8(requests.stdout).unwrap();
    assert!(
        !requests_s.contains(&sent_fyi),
        "msg requests must not list a non-request message: {requests_s}"
    );

    // `msg thread` sees all three, in send-order, and excludes the third party.
    let thread = Command::new(bin)
        .args(["--json", "msg", "thread", "bob", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        thread.status.success(),
        "msg thread failed: {}",
        String::from_utf8_lossy(&thread.stderr)
    );
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&thread.stdout).expect("thread must emit a JSON array");
    let ids: Vec<&str> = rows.iter().map(|r| r["msg_id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![request.as_str(), sent_fyi.as_str(), reply.as_str()],
        "thread must be both directions in send-order"
    );
    assert!(
        !ids.contains(&unrelated.as_str()),
        "thread must exclude messages with other actors"
    );

    let dirs: Vec<&str> = rows
        .iter()
        .map(|r| r["direction"].as_str().unwrap())
        .collect();
    assert_eq!(dirs, vec!["in", "out", "in"]);
    assert!(
        rows[0]["ack_ts"].is_string(),
        "acked request must retain its ack in the thread view"
    );
    assert_eq!(rows[0]["request_state"].as_str(), Some("open"));

    // The same fold is available to library consumers.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let convo = state.conversation_between("alice", "bob");
    assert_eq!(convo.len(), 3);
    assert_eq!(
        state.conversation_between("bob", "alice").len(),
        3,
        "conversation_between must be symmetric in its arguments"
    );
    assert_eq!(state.conversation_between("alice", "dave").len(), 0);
}

#[test]
fn msg_thread_filters_by_issue_and_kind() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "auth bug", "alice");
    let bin = mote_bin();

    for (kind, body, with_issue) in [
        ("fyi", "on the auth bug now", true),
        ("note", "unrelated chatter", false),
    ] {
        let mut args = vec![
            "msg", "send", "--to", "bob", "--kind", kind, body, "--actor", "alice",
        ];
        if with_issue {
            args.extend_from_slice(&["--issue", &issue]);
        }
        let out = Command::new(bin)
            .args(&args)
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(out.status.success());
    }

    let by_issue = Command::new(bin)
        .args([
            "msg", "thread", "bob", "--issue", &issue, "--actor", "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s = String::from_utf8(by_issue.stdout).unwrap();
    assert!(s.contains("on the auth bug now"), "{s}");
    assert!(!s.contains("unrelated chatter"), "{s}");

    let by_kind = Command::new(bin)
        .args(["msg", "thread", "bob", "--kind", "note", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s = String::from_utf8(by_kind.stdout).unwrap();
    assert!(s.contains("unrelated chatter"), "{s}");
    assert!(!s.contains("on the auth bug now"), "{s}");
}
