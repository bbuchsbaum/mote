//! Discussion routing (issue #2) and parallel-session coordination (issue #3):
//! board-to-bead promotion, pinned summaries and decisions, routing state, per
//! session identity leases, claim announcements, and the in-flight dashboard.

use std::process::Command;

use tempfile::TempDir;

use mote::state::RouteState;
use mote::{reducer, repo::Store};

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

/// Run a mote subcommand as `actor`, asserting success and returning stdout.
fn run(td: &TempDir, actor: &str, args: &[&str]) -> String {
    let out = Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "`mote {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn run_full(td: &TempDir, actor: &str, args: &[&str]) -> std::process::Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(td.path())
        .output()
        .unwrap()
}

fn post(td: &TempDir, actor: &str, topic: &str, body: &str) -> String {
    run(
        td,
        actor,
        &["discuss", "post", "--topic", topic, "--body", body],
    )
}

/// Pull a value out of a `session start` activation line. The value is
/// shell-quoted, because the line is meant to be `eval`ed.
fn export_value(stdout: &str, var: &str) -> String {
    let prefix = format!("export {var}=");
    let raw = stdout
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no `{prefix}` line in:\n{stdout}"));
    assert!(
        raw.starts_with('\'') && raw.ends_with('\''),
        "{var} value is not shell-quoted: {raw}"
    );
    raw[1..raw.len() - 1].replace(r"'\''", "'")
}

fn start_session(td: &TempDir, actor: &str, extra: &[&str]) -> String {
    let mut args = vec!["session", "start", "--as", actor];
    args.extend_from_slice(extra);
    export_value(&run(td, actor, &args), "MOTE_SESSION")
}

// ---------------------------------------------------------------- routing

#[test]
fn discuss_post_reports_topic_post_count_for_readback() {
    let td = TempDir::new().unwrap();
    init_store(&td);

    let out = run_full(
        &td,
        "alice",
        &["discuss", "post", "--topic", "roadmap", "--body", "first"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("(posts=1)"), "stderr:\n{stderr}");

    let json = run(
        &td,
        "alice",
        &[
            "--json", "discuss", "post", "--topic", "roadmap", "--body", "second",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["posts"], 2);
    assert_eq!(v["visible_in_list"], true);
    assert_eq!(v["post_kind"], "post");
}

#[test]
fn topic_new_without_body_reports_zero_posts_and_how_to_fix_it() {
    let td = TempDir::new().unwrap();
    init_store(&td);

    let out = run_full(
        &td,
        "alice",
        &["discuss", "topic", "new", "empty", "--title", "Empty"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("posts=0"), "stderr:\n{stderr}");
    assert!(stderr.contains("mote discuss post"), "stderr:\n{stderr}");

    let json = run(
        &td,
        "alice",
        &[
            "--json",
            "discuss",
            "topic",
            "new",
            "seeded",
            "--body",
            "opening argument",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["posts"], 1);
    assert_eq!(v["visible_in_list"], true);
    assert!(v["initial_post_id"].as_str().unwrap().starts_with("post-"));
}

#[test]
fn needs_bead_then_route_moves_a_post_off_the_unrouted_list() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let post_id = post(&td, "alice", "roadmap", "we should split the reducer");
    let bead = run(&td, "alice", &["new", "split the reducer"]);

    run(&td, "alice", &["discuss", "needs-bead", &post_id]);
    let unrouted = run(&td, "alice", &["discuss", "unrouted"]);
    assert!(unrouted.contains(&post_id), "unrouted:\n{unrouted}");

    run(
        &td,
        "alice",
        &["discuss", "route", &post_id, "--issue", &bead],
    );
    let unrouted = run(&td, "alice", &["discuss", "unrouted"]);
    assert!(
        !unrouted.contains(&post_id),
        "routed post still listed:\n{unrouted}"
    );

    let state = reducer::replay_store(&store).unwrap();
    let record = state.board_posts.get(&post_id).unwrap();
    assert_eq!(record.route.state, RouteState::Routed);
    assert!(record.route.issues.contains(&bead));

    // The bead carries the provenance too, so the link is visible from either side.
    let (posts, _) = state.discussion_sources_for(&bead);
    assert_eq!(posts.len(), 1);
    let history = run(&td, "alice", &["show", &bead]);
    assert!(
        history.contains("routed from discussion"),
        "show output:\n{history}"
    );
}

#[test]
fn routing_accumulates_links_and_resolve_clears_the_queue() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let post_id = post(&td, "alice", "roadmap", "two pieces of work here");
    let first = run(&td, "alice", &["new", "piece one"]);
    let second = run(&td, "alice", &["new", "piece two"]);

    run(
        &td,
        "alice",
        &["discuss", "route", &post_id, "--issue", &first],
    );
    run(
        &td,
        "alice",
        &["discuss", "route", &post_id, "--issue", &second],
    );

    let state = reducer::replay_store(&store).unwrap();
    let record = state.board_posts.get(&post_id).unwrap();
    assert_eq!(
        record.route.issues.len(),
        2,
        "second route erased the first"
    );

    run(&td, "alice", &["discuss", "resolve", &post_id]);
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.board_posts.get(&post_id).unwrap().route.state,
        RouteState::Resolved
    );
}

#[test]
fn topic_level_routing_is_tracked_separately_from_posts() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    post(&td, "alice", "roadmap", "opening");
    let bead = run(&td, "alice", &["new", "roadmap work"]);

    run(
        &td,
        "alice",
        &["discuss", "route", "--topic", "roadmap", "--issue", &bead],
    );
    let state = reducer::replay_store(&store).unwrap();
    let topic = state.board_topics.get("roadmap").unwrap();
    assert_eq!(topic.route.state, RouteState::Routed);
    assert!(topic.route.issues.contains(&bead));

    let json = run(&td, "alice", &["--json", "discuss", "topics"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v[0]["route_state"], "routed");
}

#[test]
fn route_rejects_unknown_bead_and_ambiguous_targets() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let post_id = post(&td, "alice", "roadmap", "opening");

    let missing = run_full(
        &td,
        "alice",
        &["discuss", "route", &post_id, "--issue", "bd-nope"],
    );
    assert_eq!(missing.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("does not exist"), "stderr:\n{stderr}");

    let neither = run_full(&td, "alice", &["discuss", "needs-bead"]);
    assert_eq!(neither.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&neither.stderr);
    assert!(stderr.contains("post id or --topic"), "stderr:\n{stderr}");
}

#[test]
fn promote_creates_a_bead_carrying_board_provenance_and_routes_the_post() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let post_id = post(
        &td,
        "alice",
        "roadmap",
        "the reducer needs splitting\nand the details follow",
    );

    let json = run(
        &td,
        "alice",
        &[
            "--json",
            "discuss",
            "promote",
            &post_id,
            "--priority",
            "1",
            "--tag",
            "reducer",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let bead = v["id"].as_str().unwrap().to_string();
    assert_eq!(v["route_state"], "routed");
    assert_eq!(v["topic"], "roadmap");
    // Title defaults to the post's first line so promotion needs no retyping.
    assert_eq!(v["title"], "the reducer needs splitting");

    let state = reducer::replay_store(&store).unwrap();
    let record = state.beads.get(&bead).unwrap();
    assert_eq!(record.priority, 1);
    assert!(record.tags.contains("reducer"));
    assert!(
        record.body.contains(&post_id) && record.body.contains("roadmap"),
        "bead body lost board provenance: {}",
        record.body
    );
    assert!(
        state
            .board_posts
            .get(&post_id)
            .unwrap()
            .route
            .issues
            .contains(&bead)
    );
}

#[test]
fn decision_and_summary_posts_are_pinned_and_retrievable() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    post(&td, "alice", "roadmap", "opening argument");

    run(
        &td,
        "alice",
        &[
            "discuss",
            "decision",
            "--topic",
            "roadmap",
            "--body",
            "Consensus: coordination first",
        ],
    );
    run(
        &td,
        "alice",
        &[
            "discuss",
            "summary",
            "--topic",
            "roadmap",
            "--body",
            "Current state: one open question",
        ],
    );

    let state = reducer::replay_store(&store).unwrap();
    let topic = state.board_topics.get("roadmap").unwrap();
    assert_eq!(topic.decision_count, 1);
    let summary_id = topic.summary_post_id.clone().unwrap();
    // Both are pinned: they are what a late arrival needs before the thread.
    assert!(state.board_posts.get(&summary_id).unwrap().sticky);
    assert_eq!(topic.sticky_count, 2);

    // Reading the summary back needs no thread reconstruction.
    let shown = run(&td, "bob", &["discuss", "summary", "--topic", "roadmap"]);
    assert!(
        shown.contains("Current state: one open question"),
        "{shown}"
    );

    // A newer summary replaces the pointer rather than accumulating.
    run(
        &td,
        "alice",
        &[
            "discuss",
            "summary",
            "--topic",
            "roadmap",
            "--body",
            "Current state: resolved",
        ],
    );
    let state = reducer::replay_store(&store).unwrap();
    let topic = state.board_topics.get("roadmap").unwrap();
    assert_ne!(topic.summary_post_id.as_deref(), Some(summary_id.as_str()));
    let shown = run(&td, "bob", &["discuss", "summary", "--topic", "roadmap"]);
    assert!(shown.contains("Current state: resolved"), "{shown}");
}

#[test]
fn summary_on_a_topic_without_one_reports_how_to_set_it() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    post(&td, "alice", "roadmap", "opening");

    let out = run_full(&td, "bob", &["discuss", "summary", "--topic", "roadmap"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no summary for topic"), "stderr:\n{stderr}");
}

// --------------------------------------------------------------- sessions

#[test]
fn session_start_prints_an_activation_line_and_leaves_a_visible_lease() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    let out = run(
        &td,
        "ignored",
        &["session", "start", "--as", "session-a", "--label", "triage"],
    );
    // The CLI cannot mutate its parent shell, so stdout must be evalable.
    assert_eq!(export_value(&out, "MOTE_ACTOR"), "session-a");
    let session_id = export_value(&out, "MOTE_SESSION");

    let state = reducer::replay_store(&store).unwrap();
    let record = state.sessions.get(&session_id).unwrap();
    assert_eq!(record.actor, "session-a");
    assert_eq!(record.label.as_deref(), Some("triage"));
    assert!(record.pid.is_some());

    let listed = run(&td, "session-a", &["session", "list"]);
    assert!(listed.contains(&session_id), "list:\n{listed}");

    run(&td, "session-a", &["session", "end", &session_id]);
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.sessions.get(&session_id).unwrap().ended_ts.is_some());
    let listed = run(&td, "session-a", &["session", "list"]);
    assert!(
        !listed.contains(&session_id),
        "ended session listed:\n{listed}"
    );
}

#[test]
fn session_renew_extends_the_lease_without_minting_a_new_identity() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let session_id = start_session(&td, "a", &["--ttl", "60"]);
    let before = reducer::replay_store(&store)
        .unwrap()
        .sessions
        .get(&session_id)
        .unwrap()
        .lease_until_ts
        .clone();

    run(
        &td,
        "a",
        &["session", "renew", "--id", &session_id, "--ttl", "7200"],
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.sessions.len(), 1, "renew minted a second session");
    assert!(state.sessions.get(&session_id).unwrap().lease_until_ts > before);
}

#[test]
fn a_session_cannot_be_renewed_or_ended_under_another_actor() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let session_id = start_session(&td, "a", &[]);

    // The commands must publish as the invoker, so the reducer's ownership
    // check is reachable rather than bypassed by an op that claims to be `a`.
    for args in [
        vec!["session", "renew", "--id", session_id.as_str()],
        vec!["session", "end", session_id.as_str()],
    ] {
        let out = run_full(&td, "b", &args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "`mote {}` as a non-owner should be rejected: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("belongs to a, not b"), "stderr:\n{stderr}");
    }

    let state = reducer::replay_store(&store).unwrap();
    let record = state.sessions.get(&session_id).unwrap();
    assert_eq!(record.actor, "a");
    assert!(record.ended_ts.is_none(), "a non-owner ended the session");

    // Publishing the op directly is rejected for the same reason.
    let op = mote::op::make_session_start(
        "b".into(),
        session_id.clone(),
        60,
        None,
        None,
        jiff::Timestamp::now(),
    );
    let name = mote::publish::publish_op(&store, &op).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    assert_eq!(state.sessions.get(&session_id).unwrap().actor, "a");
}

#[test]
fn ending_a_session_is_terminal() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let session_id = start_session(&td, "a", &[]);
    run(&td, "a", &["session", "end", &session_id]);

    // Renewing an ended session would make `session end` a suggestion, and
    // would let a stale lease reappear after the process behind it is gone.
    let out = run_full(&td, "a", &["session", "renew", "--id", &session_id]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("has ended"), "stderr:\n{stderr}");

    let state = reducer::replay_store(&store).unwrap();
    let record = state.sessions.get(&session_id).unwrap();
    assert!(record.ended_ts.is_some(), "ended session was resurrected");
    assert!(
        state
            .live_sessions("2000-01-01T00:00:00.000000Z")
            .is_empty()
    );
}

/// Apply an activation block exactly as the documented `eval "$(...)"` flow
/// does, and report the resulting `MOTE_ACTOR`. The block is piped through
/// stdin so this harness cannot expand it before the shell evaluates it.
fn eval_activation(cwd: &std::path::Path, activation: &str) -> String {
    use std::io::Write;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(r#"eval "$(cat)"; printf '%s' "$MOTE_ACTOR""#)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(activation.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn session_activation_lines_survive_the_shell_verbatim() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    // A name with a space must not truncate on eval: the shell would then
    // publish under a different identity than the lease records, which is the
    // exact divergence session leases exist to prevent.
    let spaced = "claude session 2";
    let out = run(&td, "x", &["session", "start", "--as", spaced]);
    assert_eq!(export_value(&out, "MOTE_ACTOR"), spaced);
    assert_eq!(
        eval_activation(td.path(), &out),
        spaced,
        "eval of the activation line changed the identity"
    );

    // Shell metacharacters in a persisted actor name must not execute. That
    // file travels with a checkout, so it is not trusted input.
    let hostile = "alice$(touch pwned)";
    let out = run(&td, hostile, &["session", "start"]);
    assert_eq!(eval_activation(td.path(), &out), hostile);
    assert!(
        !td.path().join("pwned").exists(),
        "activation line executed embedded shell"
    );
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .sessions
            .values()
            .any(|s| s.actor == hostile)
    );
}

#[test]
fn doctor_flags_the_shared_identity_failures_that_actually_hide_collisions() {
    let td = TempDir::new().unwrap();
    init_store(&td);

    // A distinct per-session name with nothing overlapping is clean, even
    // though every mote invocation ran as a separate process.
    let quiet = run(&td, "session-a", &["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_str(&quiet).unwrap();
    assert_eq!(
        v["warnings"].as_array().unwrap().len(),
        0,
        "sequential commands must not look like concurrent sessions: {}",
        v["warnings"]
    );

    // A generic identity is called out: it is what two agents pick by default.
    let sentinel = run(&td, "claude", &["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_str(&sentinel).unwrap();
    assert!(
        v["warnings"][0]
            .as_str()
            .unwrap()
            .contains("generic default"),
        "warnings: {}",
        v["warnings"]
    );

    // Same-actor reservations never conflict, so an overlap between two of them
    // is exactly the collision `preflight` and `who-has` cannot see.
    let bead = run(&td, "shared", &["new", "shared work"]);
    run(&td, "shared", &["reserve", "--issue", &bead, "src/lib.rs"]);
    run(&td, "shared", &["reserve", "--issue", &bead, "src/lib.rs"]);
    let overlapped = run(&td, "shared", &["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_str(&overlapped).unwrap();
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("both hold `src/lib.rs`")),
        "warnings: {warnings:?}"
    );
    // A coordination hazard is not a broken store.
    assert_eq!(v["ok"], true);
}

#[test]
fn doctor_flags_concurrent_session_leases_sharing_one_actor() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    run(&td, "shared", &["session", "start", "--as", "shared"]);
    run(&td, "shared", &["session", "start", "--as", "shared"]);

    let json = run(&td, "shared", &["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let warnings = v["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap()
            .contains("live session leases share actor")),
        "warnings: {warnings:?}"
    );
}

// ------------------------------------------------------- begin + in-flight

#[test]
fn begin_announce_posts_the_claim_to_the_source_topic() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let bead = run(&td, "alice", &["new", "auth work"]);
    post(&td, "alice", "roadmap", "opening");

    let out = run_full(
        &td,
        "alice",
        &[
            "begin",
            &bead,
            "--paths",
            "src/auth.rs",
            "--announce",
            "roadmap",
        ],
    );
    assert!(out.status.success());
    let rv = String::from_utf8(out.stdout).unwrap().trim().to_string();

    let state = reducer::replay_store(&store).unwrap();
    let claim_post = state
        .board_posts_for(Some("roadmap"))
        .into_iter()
        .find(|p| p.body.contains(&bead))
        .expect("no claim announcement on the source topic");
    assert!(claim_post.body.contains(&rv), "body: {}", claim_post.body);
    assert!(
        claim_post.body.contains("src/auth.rs"),
        "body: {}",
        claim_post.body
    );
}

#[test]
fn begin_rejects_a_malformed_announce_topic_before_reserving_anything() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let bead = run(&td, "alice", &["new", "auth work"]);

    let out = run_full(
        &td,
        "alice",
        &["begin", &bead, "--paths", "src/auth.rs", "--announce", "  "],
    );
    assert_eq!(out.status.code(), Some(3));
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state.reservations.is_empty(),
        "a bad topic left a reservation behind"
    );
}

#[test]
fn in_flight_answers_the_collision_question_in_one_invocation() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bead = run(&td, "session-a", &["new", "auth work"]);
    post(&td, "session-a", "roadmap", "opening");
    run(&td, "session-a", &["session", "start", "--as", "session-a"]);
    run(
        &td,
        "session-a",
        &["begin", &bead, "--paths", "src/auth.rs"],
    );

    let json = run(&td, "session-a", &["--json", "in-flight", "--no-git"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(v["reservations"].as_array().unwrap().len(), 1);
    assert_eq!(v["doing"][0]["id"], bead.as_str());
    assert_eq!(v["doing"][0]["claimed_by"], "session-a");
    assert_eq!(v["topics"][0]["topic"], "roadmap");
    // --no-git keeps the view purely replay-derived.
    assert_eq!(v["recent_commits_advisory"].as_array().unwrap().len(), 0);

    let human = run(&td, "session-a", &["in-flight", "--no-git"]);
    for section in ["SESSIONS", "RESERVATIONS", "DOING", "ACTIVE TOPICS"] {
        assert!(human.contains(section), "missing {section} in:\n{human}");
    }
}

#[test]
fn in_flight_survives_an_absurd_window() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    post(&td, "alice", "roadmap", "opening");

    // Saturate rather than overflow: a huge window means "everything".
    let json = run(
        &td,
        "alice",
        &[
            "--json",
            "in-flight",
            "--no-git",
            "--minutes",
            "200000000000000000",
        ],
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["topics"].as_array().unwrap().len(), 1);
}

#[test]
fn summary_rejects_two_sources_of_text_like_every_other_post_command() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    post(&td, "alice", "roadmap", "opening");

    let out = run_full(
        &td,
        "alice",
        &[
            "discuss",
            "summary",
            "--topic",
            "roadmap",
            "--body",
            "FROM BODY",
            "FROM POSITIONAL",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(3),
        "one of the two texts was dropped"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not both"), "stderr:\n{stderr}");
}

#[test]
fn doctor_reports_one_finding_per_colliding_reservation_pair() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bead = run(&td, "shared", &["new", "shared work"]);
    // A multi-path reservation against a directory reservation overlaps twice,
    // but it is one collision, not two.
    run(
        &td,
        "shared",
        &["reserve", "--issue", &bead, "src/a.rs", "src/b.rs"],
    );
    run(&td, "shared", &["reserve", "--issue", &bead, "src/"]);

    let json = run(&td, "shared", &["--json", "doctor"]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let overlaps: Vec<_> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w.as_str().unwrap().contains("both hold"))
        .collect();
    assert_eq!(overlaps.len(), 1, "warnings: {:?}", v["warnings"]);
}

#[test]
fn show_reports_the_discussion_a_bead_came_from() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let post_id = post(&td, "alice", "roadmap", "split the parser");
    let bead = run(&td, "alice", &["discuss", "promote", &post_id]);

    let human = run(&td, "alice", &["show", &bead]);
    assert!(human.contains(&post_id), "show output:\n{human}");
    assert!(
        human.contains("promoted from discussion post"),
        "promote left no note in history:\n{human}"
    );

    let json = run(&td, "alice", &["--json", "show", &bead]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["discussion_sources"]["posts"][0]["post_id"], post_id);
    assert_eq!(v["discussion_sources"]["posts"][0]["topic"], "roadmap");

    // Promoting again is allowed but says what already exists.
    let again = run_full(&td, "alice", &["discuss", "promote", &post_id]);
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("already routed to"), "stderr:\n{stderr}");
}

#[test]
fn in_flight_window_excludes_stale_topics() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    post(&td, "alice", "roadmap", "opening");

    let fresh = run(&td, "alice", &["--json", "in-flight", "--no-git"]);
    let v: serde_json::Value = serde_json::from_str(&fresh).unwrap();
    assert_eq!(v["topics"].as_array().unwrap().len(), 1);

    // A zero-length window admits nothing, proving the filter is applied.
    let none = run(
        &td,
        "alice",
        &["--json", "in-flight", "--no-git", "--minutes", "0"],
    );
    let v: serde_json::Value = serde_json::from_str(&none).unwrap();
    assert_eq!(v["topics"].as_array().unwrap().len(), 0);
}
