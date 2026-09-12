use std::fs;
use std::process::{Command, Output};
use std::time::Instant;

use jiff::Timestamp;
use mote::discussion_pulse::{DiscussionPulseParameters, build_discussion_pulse};
use mote::state::{BoardPostRecord, BoardTopicRecord, RouteRecord, State};
use tempfile::TempDir;

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn run_output(temp: &TempDir, actor: &str, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(temp.path())
        .output()
        .unwrap()
}

fn run(temp: &TempDir, actor: &str, args: &[&str]) -> String {
    let output = run_output(temp, actor, args);
    assert!(
        output.status.success(),
        "mote {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run_json(temp: &TempDir, actor: &str, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&run(temp, actor, args)).unwrap()
}

fn op_count(temp: &TempDir) -> usize {
    fs::read_dir(temp.path().join(".mote/ops"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .count()
}

fn scenario() -> TempDir {
    let temp = TempDir::new().unwrap();
    run(&temp, "admin", &["init"]);
    for index in 0..4 {
        let actor = if index % 2 == 0 { "alice" } else { "bob" };
        run(
            &temp,
            actor,
            &[
                "discuss",
                "post",
                "--topic",
                "busy",
                "--body",
                &format!("busy post {index}"),
            ],
        );
    }
    run(
        &temp,
        "carol",
        &[
            "discuss",
            "post",
            "--topic",
            "quiet",
            "--body",
            "one easy-to-miss post",
        ],
    );
    temp
}

#[test]
fn pulse_cli_is_actionable_configurable_and_passive() {
    let temp = scenario();
    let before = op_count(&temp);
    let pulse = run_json(
        &temp,
        "viewer",
        &[
            "--json",
            "discuss",
            "pulse",
            "--short-window",
            "2m",
            "--burst-window",
            "20m",
            "--active-window",
            "2h",
            "--attention-window",
            "30m",
            "--burst-posts",
            "4",
            "--burst-actors",
            "2",
        ],
    );

    assert_eq!(pulse["schema"], "mote.discussion-pulse.v1");
    assert_eq!(pulse["actor"], "viewer");
    assert_eq!(pulse["parameters"]["short_window_s"], 120);
    assert_eq!(pulse["parameters"]["burst_window_s"], 1_200);
    assert_eq!(pulse["parameters"]["active_window_s"], 7_200);
    assert_eq!(pulse["parameters"]["attention_window_s"], 1_800);
    assert!(
        pulse["definitions"]["solitary_new"]
            .as_str()
            .unwrap()
            .contains("exactly one current active external post")
    );
    assert_eq!(pulse["traversal"]["posts_scanned"], 5);
    let active = pulse["active_now"].as_array().unwrap();
    let busy = active
        .iter()
        .find(|topic| topic["topic"] == "busy")
        .unwrap();
    assert_eq!(busy["posts_5m"], 4);
    assert_eq!(busy["active_posts_60m"], 4);
    assert_eq!(busy["raw_posts_60m"], 4);
    assert_eq!(busy["distinct_authors_60m"], 2);
    assert_eq!(busy["burst"], true);
    assert!(busy["last_post_id"].as_str().unwrap().starts_with("post-"));
    assert!(busy["last_activity_ts"].is_string());
    let quiet = pulse["needs_eyes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|topic| topic["topic"] == "quiet")
        .unwrap();
    assert_eq!(quiet["solitary_new_post_ids"].as_array().unwrap().len(), 1);

    let human = run(&temp, "viewer", &["discuss", "pulse"]);
    assert!(human.contains("ACTIVE NOW"));
    assert!(human.contains("NEEDS EYES"));
    assert!(human.contains("quiet"));
    assert!(human.contains("solitary=post-"));
    let help = run(&temp, "viewer", &["discuss", "pulse", "--help"]);
    assert!(help.contains("solitary-new means exactly one current active external post"));
    assert_eq!(op_count(&temp), before, "pulse must not publish an op");
}

#[test]
fn monitoring_surfaces_share_the_pulse_schema_without_consuming_unread() {
    let temp = scenario();
    let before = op_count(&temp);

    let board = run_json(&temp, "viewer", &["--json", "board"]);
    let inflight = run_json(&temp, "viewer", &["--json", "in-flight", "--no-git"]);
    let actor = run_json(&temp, "viewer", &["--json", "actor", "status"]);
    for value in [&board, &inflight, &actor] {
        assert_eq!(
            value["discussion_pulse"]["schema"],
            "mote.discussion-pulse.v1"
        );
        assert_eq!(value["discussion_pulse"]["totals"]["solitary_new_posts"], 1);
    }

    let store = mote::repo::Store::discover(temp.path()).unwrap();
    let state = mote::reducer::replay_store(&store).unwrap();
    let now = mote::ids::format_rfc3339(Timestamp::now());
    let watch = mote::watch::snapshot_value(&state, Some("viewer"), &now);
    assert_eq!(
        watch["discussion_pulse"]["schema"],
        "mote.discussion-pulse.v1"
    );
    assert_eq!(
        op_count(&temp),
        before,
        "monitoring surfaces must not consume unread or publish ops"
    );
}

#[test]
fn invalid_pulse_parameters_fail_before_mutation() {
    let temp = scenario();
    let before = op_count(&temp);
    let output = run_output(&temp, "viewer", &["discuss", "pulse", "--burst-posts", "0"]);
    assert_eq!(output.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&output.stderr).contains("greater than zero"));
    assert_eq!(op_count(&temp), before);
}

fn topic(name: &str) -> BoardTopicRecord {
    BoardTopicRecord {
        topic: name.into(),
        title: name.into(),
        body: String::new(),
        created_by: "seed".into(),
        created_ts: "2026-09-12T10:00:00Z".into(),
        created_op_id: format!("op-topic-{name}"),
        explicit: true,
        last_activity_ts: "2026-09-12T11:55:00Z".into(),
        last_activity_op_id: format!("op-last-{name}"),
        post_count: 10,
        sticky_count: 0,
        decision_count: 0,
        summary_post_id: None,
        route: RouteRecord::default(),
    }
}

fn post(index: usize, topic: &str) -> BoardPostRecord {
    BoardPostRecord {
        post_id: format!("post-{index:06}"),
        from: format!("actor-{}", index % 10),
        topic: topic.into(),
        body: "load".into(),
        reply_to: None,
        post_kind: "post".into(),
        answers: Vec::new(),
        explicit_notify: Vec::new(),
        notification_recipients: Vec::new(),
        idempotency_key: None,
        sticky: false,
        sticky_op_id: None,
        superseded_by: None,
        superseded_op_id: None,
        supersedes: Vec::new(),
        retracted: false,
        retraction_reason: None,
        retracted_op_id: None,
        route: RouteRecord::default(),
        sent_ts: "2026-09-12T11:55:00Z".into(),
        sent_op_id: format!("op-{index:06}"),
    }
}

#[test]
#[ignore = "explicit 10k-topic/100k-post scale receipt"]
fn performance_receipt_is_linear_and_bounded_at_target_scale() {
    const TOPICS: usize = 10_000;
    const POSTS: usize = 100_000;
    let mut state = State::default();
    for index in 0..TOPICS {
        let name = format!("topic-{index:05}");
        state.board_topics.insert(name.clone(), topic(&name));
    }
    for index in 0..POSTS {
        let name = format!("topic-{:05}", index % TOPICS);
        let record = post(index, &name);
        state.board_posts.insert(record.post_id.clone(), record);
    }

    let started = Instant::now();
    let pulse = build_discussion_pulse(
        &state,
        Some("viewer"),
        "2026-09-12T12:00:00Z".parse().unwrap(),
        DiscussionPulseParameters::default(),
    )
    .unwrap();
    let elapsed = started.elapsed();
    eprintln!(
        "discussion-pulse receipt: topics={TOPICS} posts={POSTS} elapsed_ms={}",
        elapsed.as_millis()
    );
    assert_eq!(pulse.traversal.topics_scanned, TOPICS);
    assert_eq!(pulse.traversal.posts_scanned, POSTS);
    assert_eq!(pulse.traversal.reply_edges_scanned, 0);
    assert_eq!(pulse.active_now.len(), TOPICS);
    assert!(
        elapsed.as_secs_f64() < 5.0,
        "100k-post pulse exceeded 5s: {elapsed:?}"
    );
}
