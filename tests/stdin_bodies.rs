use std::io::Write;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

use mote::op::Status;
use mote::reducer;
use mote::repo::Store;

const LITERAL: &str =
    "`ticks` $(not-run) <tag> \"double\" 'single'\nUnicode: λ 🧠\n-leading-dash\n";

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

fn run_with_stdin(td: &TempDir, args: &[&str], input: &[u8]) -> Output {
    let mut child = Command::new(mote_bin())
        .args(args)
        .current_dir(td.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn output_text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .trim()
        .to_string()
}

fn assert_ok(output: &Output, args: &[&str]) {
    assert!(
        output.status.success(),
        "command failed: mote {}\nstdout={}\nstderr={}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_ok(td: &TempDir, args: &[&str]) -> String {
    let output = run(td, args);
    assert_ok(&output, args);
    output_text(&output)
}

fn run_stdin_ok(td: &TempDir, args: &[&str], input: &str) -> String {
    let output = run_with_stdin(td, args, input.as_bytes());
    assert_ok(&output, args);
    output_text(&output)
}

fn init(td: &TempDir) {
    run_ok(td, &["init"]);
}

fn state(td: &TempDir) -> mote::state::State {
    let store = Store::open(&td.path().join(".mote")).unwrap();
    reducer::replay_store(&store).unwrap()
}

#[test]
fn body_options_read_literal_stdin_for_new_and_discussion_commands() {
    let td = TempDir::new().unwrap();
    init(&td);

    let new_args = ["new", "stdin bead", "--body", "-", "--actor", "alice"];
    let bead_id = run_stdin_ok(&td, &new_args, LITERAL);
    assert_eq!(state(&td).beads[&bead_id].body, LITERAL);

    let topic_args = [
        "discuss",
        "topic",
        "new",
        "stdin-topic",
        "--body",
        "-",
        "--actor",
        "alice",
    ];
    run_stdin_ok(&td, &topic_args, LITERAL);

    let post_args = [
        "discuss",
        "post",
        "--topic",
        "stdin-topic",
        "--body",
        "-",
        "--actor",
        "alice",
    ];
    let source_post = run_stdin_ok(&td, &post_args, LITERAL);

    let route_args = [
        "discuss",
        "route",
        &source_post,
        "--issue",
        &bead_id,
        "--note",
        "-",
        "--actor",
        "alice",
    ];
    run_stdin_ok(&td, &route_args, LITERAL);

    let decision_args = [
        "discuss",
        "decision",
        "--topic",
        "stdin-topic",
        "--body",
        "-",
        "--actor",
        "alice",
    ];
    run_stdin_ok(&td, &decision_args, LITERAL);

    let summary_args = [
        "discuss",
        "summary",
        "--topic",
        "stdin-topic",
        "--body",
        "-",
        "--actor",
        "alice",
    ];
    run_stdin_ok(&td, &summary_args, LITERAL);

    let promote_args = [
        "discuss",
        "promote",
        &source_post,
        "--body",
        "-",
        "--actor",
        "alice",
    ];
    let promoted = run_stdin_ok(&td, &promote_args, LITERAL);

    let state = state(&td);
    assert_eq!(state.board_posts.len(), 4);
    assert!(state.board_posts.values().all(|post| post.body == LITERAL));
    assert!(state.beads[&bead_id].notes[0].text.ends_with(LITERAL));
    assert_eq!(
        state.beads[&promoted].body,
        format!("{LITERAL}\n\npromoted from discussion post {source_post} in topic stdin-topic")
    );
}

#[test]
fn positional_text_commands_require_explicit_stdin_and_preserve_literal_dash() {
    let td = TempDir::new().unwrap();
    init(&td);
    let bead_id = run_ok(&td, &["new", "stdin text", "--actor", "alice"]);

    let note_args = [
        "note", &bead_id, "--kind", "progress", "--stdin", "--actor", "alice",
    ];
    run_stdin_ok(&td, &note_args, LITERAL);

    let send_args = [
        "msg", "send", "--to", "bob", "--kind", "request", "--stdin", "--actor", "alice",
    ];
    let request_id = run_stdin_ok(&td, &send_args, LITERAL);

    let reply_args = ["msg", "reply", &request_id, "--stdin", "--actor", "bob"];
    let reply_id = run_stdin_ok(&td, &reply_args, LITERAL);

    let dash_args = ["msg", "send", "--to", "bob", "-", "--actor", "alice"];
    let dash_id = run_ok(&td, &dash_args);

    let state = state(&td);
    assert_eq!(state.beads[&bead_id].notes[0].text, LITERAL);
    assert_eq!(state.messages[&request_id].body, LITERAL);
    assert_eq!(state.messages[&reply_id].body, LITERAL);
    assert_eq!(state.messages[&dash_id].body, "-");

    let missing_args = ["msg", "send", "--to", "bob", "--actor", "alice"];
    let missing = run_with_stdin(&td, &missing_args, LITERAL.as_bytes());
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("required"),
        "stderr={}",
        String::from_utf8_lossy(&missing.stderr)
    );

    let ambiguous_args = [
        "msg",
        "send",
        "--to",
        "bob",
        "positional",
        "--stdin",
        "--actor",
        "alice",
    ];
    let ambiguous = run_with_stdin(&td, &ambiguous_args, LITERAL.as_bytes());
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr).contains("cannot be used with"),
        "stderr={}",
        String::from_utf8_lossy(&ambiguous.stderr)
    );
}

#[test]
fn compound_notes_read_stdin_before_publishing_state() {
    let td = TempDir::new().unwrap();
    init(&td);
    let bead_id = run_ok(&td, &["new", "compound stdin", "--actor", "alice"]);

    let begin_args = [
        "begin",
        &bead_id,
        "--paths",
        "src/example.rs",
        "--note",
        "-",
        "--actor",
        "alice",
    ];
    run_stdin_ok(&td, &begin_args, "begin note\n");

    let handoff_args = [
        "handoff",
        &bead_id,
        "--to",
        "bob",
        "--note",
        "-",
        "--release",
        "--actor",
        "alice",
    ];
    run_stdin_ok(&td, &handoff_args, "handoff note\n");

    let done_args = ["done", &bead_id, "--note", "-", "--actor", "bob"];
    run_stdin_ok(&td, &done_args, "done note\n");

    let state = state(&td);
    let bead = &state.beads[&bead_id];
    assert_eq!(bead.status, Status::Closed);
    assert_eq!(
        bead.notes
            .iter()
            .map(|note| note.text.as_str())
            .collect::<Vec<_>>(),
        ["begin note\n", "handoff note\n", "done note\n"]
    );
}

#[test]
fn stdin_validation_rejects_ambiguous_empty_and_non_utf8_input_before_publish() {
    let td = TempDir::new().unwrap();
    init(&td);

    let ambiguous_args = [
        "discuss",
        "post",
        "positional",
        "--body",
        "-",
        "--actor",
        "alice",
    ];
    let ambiguous = run_with_stdin(&td, &ambiguous_args, LITERAL.as_bytes());
    assert!(!ambiguous.status.success());
    assert!(
        String::from_utf8_lossy(&ambiguous.stderr).contains("not both"),
        "stderr={}",
        String::from_utf8_lossy(&ambiguous.stderr)
    );

    let empty_args = ["discuss", "post", "--body", "-", "--actor", "alice"];
    let empty = run_with_stdin(&td, &empty_args, b"");
    assert!(!empty.status.success());
    assert!(
        String::from_utf8_lossy(&empty.stderr).contains("must be non-empty"),
        "stderr={}",
        String::from_utf8_lossy(&empty.stderr)
    );

    let invalid_utf8_args = ["new", "invalid utf8", "--body", "-", "--actor", "alice"];
    let invalid_utf8 = run_with_stdin(&td, &invalid_utf8_args, &[0xff, 0xfe]);
    assert!(!invalid_utf8.status.success());

    let state = state(&td);
    assert!(state.board_posts.is_empty());
    assert!(state.beads.is_empty());
}
