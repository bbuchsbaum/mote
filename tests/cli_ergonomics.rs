use std::process::{Command, Output};

use mote::reducer;
use mote::repo::Store;
use tempfile::TempDir;

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

fn success(td: &TempDir, args: &[&str]) -> String {
    let output = run(td, args);
    assert!(
        output.status.success(),
        "mote {} failed\nstdout={}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn send_and_assign_are_thin_aliases_for_the_canonical_operations() {
    let td = TempDir::new().unwrap();
    success(&td, &["init"]);
    let bead = success(&td, &["new", "alias target", "--actor", "alice"]);

    let message = success(
        &td,
        &[
            "send",
            "bob",
            "please review",
            "--issue",
            &bead,
            "--kind",
            "request",
            "--idempotency-key",
            "safe-send-1",
            "--actor",
            "alice",
        ],
    );
    success(&td, &["assign", &bead, "bob", "--actor", "alice"]);

    let state = reducer::replay_store(&Store::open(&td.path().join(".mote")).unwrap()).unwrap();
    assert_eq!(state.beads[&bead].assignee.as_deref(), Some("bob"));
    let sent = &state.messages[&message];
    assert_eq!(sent.from, "alice");
    assert_eq!(sent.to, "bob");
    assert_eq!(sent.entity.as_deref(), Some(bead.as_str()));
    assert_eq!(sent.msg_kind, "request");
    assert_eq!(sent.body, "please review");
}

#[test]
fn observed_message_near_misses_name_both_safe_forms() {
    let td = TempDir::new().unwrap();
    for args in [
        &["msg", "bob", "hello"][..],
        &["msg", "--to", "bob", "hello"][..],
    ] {
        let output = run(&td, args);
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("mote send <actor> <body>"), "{stderr}");
        assert!(
            stderr.contains("mote msg send --to <actor> <body>"),
            "{stderr}"
        );
    }
}

#[test]
fn ambiguous_discussion_positionals_are_rejected_with_named_topic_recovery() {
    let td = TempDir::new().unwrap();
    let output = run(&td, &["discuss", "post", "planning", "hello"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("mote discuss post --topic <topic> <body>"),
        "{stderr}"
    );
    assert!(stderr.contains("topic `general`"), "{stderr}");
}

#[test]
fn one_discussion_positional_keeps_its_existing_body_meaning() {
    let td = TempDir::new().unwrap();
    success(&td, &["init"]);
    let post = success(&td, &["discuss", "post", "planning", "--actor", "alice"]);
    let state = reducer::replay_store(&Store::open(&td.path().join(".mote")).unwrap()).unwrap();
    assert_eq!(state.board_posts[&post].topic, "general");
    assert_eq!(state.board_posts[&post].body, "planning");
}

#[test]
fn help_exposes_shortest_safe_forms_without_hiding_canonical_send() {
    for args in [
        &["send", "--help"][..],
        &["assign", "--help"][..],
        &["msg", "send", "--help"][..],
    ] {
        let output = Command::new(mote_bin()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
    }
}
