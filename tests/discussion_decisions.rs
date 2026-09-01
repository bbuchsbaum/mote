//! Contract tests for cited discussion decisions and tracked open questions.

use std::process::{Command, Output};

use jiff::Timestamp;
use tempfile::TempDir;

use mote::events::{EventFilter, accepted_events};
use mote::ids;
use mote::op::{
    BoardDecisionOp, BoardQuestionOp, DecisionQuestionAction, DecisionQuestionDraft,
    DiscussionReference, Op, ScalarSet, make_board_post, make_board_post_of_kind, make_board_watch,
    make_create,
};
use mote::state::{DecisionQuestionStatus, RouteState};
use mote::{publish, reducer, repo::Store};

fn timestamp(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn publish_checked(store: &Store, operation: &Op) -> String {
    let name = publish::publish_op(store, operation).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(
        state.was_accepted(name.as_str()),
        "rejected: {:?}",
        state.rejection_reason(name.as_str())
    );
    name.into_string()
}

fn publish_rejected(store: &Store, operation: &Op, fragment: &str) -> String {
    let name = publish::publish_op(store, operation).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    let reason = state.rejection_reason(name.as_str()).unwrap();
    assert!(
        reason.contains(fragment),
        "expected `{fragment}` in `{reason}`"
    );
    name.into_string()
}

fn setup() -> (TempDir, Store) {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    (temp, store)
}

fn post(store: &Store, actor: &str, topic: &str, body: &str, at: &str) -> String {
    let post_id = ids::new_post_id();
    publish_checked(
        store,
        &make_board_post(
            actor.into(),
            post_id.clone(),
            topic.into(),
            body.into(),
            None,
            timestamp(at),
        ),
    );
    post_id
}

fn issue(store: &Store, actor: &str, title: &str, at: &str) -> String {
    let issue_id = ids::new_bead_id();
    publish_checked(
        store,
        &make_create(
            actor.into(),
            issue_id.clone(),
            ScalarSet {
                title: Some(title.into()),
                ..Default::default()
            },
            timestamp(at),
        ),
    );
    issue_id
}

#[allow(clippy::too_many_arguments)]
fn decision(
    actor: &str,
    decision_id: &str,
    topic: &str,
    agreed_post_ids: Vec<String>,
    references: Vec<DiscussionReference>,
    questions: Vec<(&str, &str)>,
    notify: Vec<&str>,
    key: Option<&str>,
    at: &str,
) -> Op {
    Op::BoardDecision(BoardDecisionOp {
        v: 1,
        op: String::new(),
        ts: at.into(),
        actor: actor.into(),
        post_id: decision_id.into(),
        topic: topic.into(),
        body: "Explicitly cited decision".into(),
        agreed_post_ids,
        references,
        open_questions: questions
            .into_iter()
            .map(|(question_id, text)| DecisionQuestionDraft {
                question_id: question_id.into(),
                text: text.into(),
            })
            .collect(),
        notify: notify.into_iter().map(str::to_string).collect(),
        idempotency_key: key.map(str::to_string),
    })
}

#[allow(clippy::too_many_arguments)]
fn question_op(
    actor: &str,
    question_id: &str,
    action: DecisionQuestionAction,
    expect: &str,
    mut references: Vec<DiscussionReference>,
    note: Option<&str>,
    successor_question_id: Option<&str>,
    key: &str,
    at: &str,
) -> Op {
    references.sort();
    Op::BoardQuestion(BoardQuestionOp {
        v: 1,
        op: String::new(),
        ts: at.into(),
        actor: actor.into(),
        question_id: question_id.into(),
        action,
        expect_question: expect.into(),
        references,
        note: note.map(str::to_string),
        successor_question_id: successor_question_id.map(str::to_string),
        idempotency_key: Some(key.into()),
    })
}

#[test]
fn legacy_decisions_replay_without_structured_state() {
    let (_temp, store) = setup();
    post(
        &store,
        "alice",
        "architecture",
        "opening context",
        "2030-01-01T00:00:00Z",
    );
    publish_checked(
        &store,
        &make_board_post_of_kind(
            "alice".into(),
            ids::new_post_id(),
            "architecture".into(),
            "legacy prose conclusion".into(),
            None,
            Some("decision".into()),
            timestamp("2030-01-01T00:00:01Z"),
        ),
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.board_topics["architecture"].decision_count, 1);
    assert!(state.board_decisions.is_empty());
    assert!(state.board_questions.is_empty());
}

#[test]
fn structured_decision_is_atomic_sticky_routed_and_notified() {
    let (_temp, store) = setup();
    let first = post(
        &store,
        "alice",
        "architecture",
        "Use explicit citations.",
        "2030-01-01T00:00:00Z",
    );
    let second = post(
        &store,
        "bob",
        "architecture",
        "Answers do not imply closure.",
        "2030-01-01T00:00:01Z",
    );
    let issue_id = issue(
        &store,
        "alice",
        "decision follow-up",
        "2030-01-01T00:00:02Z",
    );
    publish_checked(
        &store,
        &make_board_watch(
            "watcher".into(),
            "architecture".into(),
            true,
            timestamp("2030-01-01T00:00:03Z"),
        ),
    );
    let decision_id = ids::new_post_id();
    let first_question = ids::new_question_id();
    let second_question = ids::new_question_id();
    let mut agreed = vec![second.clone(), first.clone()];
    agreed.sort();
    let mut references = vec![
        DiscussionReference::Url {
            url: "https://example.test/design".into(),
        },
        DiscussionReference::Issue {
            issue_id: issue_id.clone(),
        },
        DiscussionReference::Topic {
            topic: "architecture".into(),
        },
    ];
    references.sort();
    let decision_op_id = publish_checked(
        &store,
        &decision(
            "alice",
            &decision_id,
            "architecture",
            agreed.clone(),
            references.clone(),
            vec![
                (&first_question, "Which surface lands first?"),
                (&second_question, "What evidence closes rollout?"),
            ],
            vec!["carol", "alice"],
            Some("architecture-decision-1"),
            "2030-01-01T00:00:04Z",
        ),
    );

    let state = reducer::replay_store(&store).unwrap();
    let topic = &state.board_topics["architecture"];
    assert_eq!(topic.post_count, 3);
    assert_eq!(topic.sticky_count, 1);
    assert_eq!(topic.decision_count, 1);
    assert_eq!(topic.last_activity_op_id, decision_op_id);
    let post = &state.board_posts[&decision_id];
    assert!(post.sticky);
    assert_eq!(post.post_kind, "decision");
    assert_eq!(post.route.state, RouteState::Routed);
    assert!(post.route.issues.contains(&issue_id));
    assert_eq!(
        post.notification_recipients,
        vec!["carol".to_string(), "watcher".to_string()]
    );
    assert_eq!(post.explicit_notify, vec!["carol".to_string()]);
    let record = &state.board_decisions[&decision_id];
    assert_eq!(record.agreed_post_ids, agreed);
    assert_eq!(record.references, references);
    assert_eq!(
        record.question_ids,
        vec![first_question.clone(), second_question.clone()]
    );
    assert_eq!(state.board_questions[&first_question].position, 0);
    assert_eq!(state.board_questions[&second_question].position, 1);
    assert_eq!(
        state.board_questions[&first_question].status,
        DecisionQuestionStatus::Open
    );

    let events = accepted_events(
        &store,
        None,
        &EventFilter::new(&["discussion".into()], None).unwrap(),
    )
    .unwrap();
    let event = events
        .iter()
        .find(|event| {
            event.event_type == "discussion.decided"
                && event.data["post_id"].as_str() == Some(decision_id.as_str())
        })
        .expect("structured decision event");
    assert_eq!(
        event.data["notification_recipients"],
        serde_json::json!(["carol", "watcher"])
    );
}

#[test]
fn malformed_or_unresolved_citations_reject_without_partial_state() {
    let (_temp, store) = setup();
    let agreed = post(
        &store,
        "alice",
        "architecture",
        "agreed clause",
        "2030-01-01T00:00:00Z",
    );
    let other = post(
        &store,
        "alice",
        "operations",
        "cross-topic clause",
        "2030-01-01T00:00:01Z",
    );
    let before = reducer::replay_store(&store).unwrap();
    let before_architecture = before.board_topics["architecture"].post_count;

    publish_rejected(
        &store,
        &decision(
            "alice",
            &ids::new_post_id(),
            "architecture",
            vec![other],
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            "2030-01-01T00:00:02Z",
        ),
        "must be active in decision topic",
    );
    publish_rejected(
        &store,
        &decision(
            "alice",
            &ids::new_post_id(),
            "architecture",
            vec![agreed.clone()],
            vec![
                DiscussionReference::Url {
                    url: "https://example.test/repeated".into(),
                },
                DiscussionReference::Url {
                    url: "https://example.test/repeated".into(),
                },
            ],
            Vec::new(),
            Vec::new(),
            None,
            "2030-01-01T00:00:03Z",
        ),
        "sorted and unique",
    );
    publish_rejected(
        &store,
        &decision(
            "alice",
            &ids::new_post_id(),
            "architecture",
            vec![agreed.clone()],
            vec![DiscussionReference::Issue {
                issue_id: "bd-missing".into(),
            }],
            Vec::new(),
            Vec::new(),
            None,
            "2030-01-01T00:00:04Z",
        ),
        "referenced live issue",
    );
    publish_rejected(
        &store,
        &decision(
            "alice",
            &ids::new_post_id(),
            "architecture",
            vec![agreed.clone()],
            vec![DiscussionReference::Url {
                url: "file:///tmp/claim".into(),
            }],
            Vec::new(),
            Vec::new(),
            None,
            "2030-01-01T00:00:05Z",
        ),
        "absolute HTTP(S)",
    );
    publish_rejected(
        &store,
        &decision(
            "alice",
            &ids::new_post_id(),
            "architecture",
            vec![agreed],
            Vec::new(),
            vec![("question-not-a-ulid", "invalid id")],
            Vec::new(),
            None,
            "2030-01-01T00:00:06Z",
        ),
        "question-ULID",
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.board_topics["architecture"].post_count,
        before_architecture
    );
    assert_eq!(state.board_topics["architecture"].decision_count, 0);
    assert!(state.board_decisions.is_empty());
    assert!(state.board_questions.is_empty());
}

#[test]
fn answers_are_candidates_and_lifecycle_is_explicit_and_authorized() {
    let (_temp, store) = setup();
    let agreed = post(
        &store,
        "alice",
        "architecture",
        "agreed clause",
        "2030-01-01T00:00:00Z",
    );
    let answer = post(
        &store,
        "bob",
        "architecture",
        "candidate answer",
        "2030-01-01T00:00:01Z",
    );
    let wrong_topic = post(
        &store,
        "bob",
        "operations",
        "not an in-topic answer",
        "2030-01-01T00:00:02Z",
    );
    let decision_id = ids::new_post_id();
    let question_id = ids::new_question_id();
    let opened = publish_checked(
        &store,
        &decision(
            "alice",
            &decision_id,
            "architecture",
            vec![agreed],
            Vec::new(),
            vec![(&question_id, "Does the rail carry relations?")],
            Vec::new(),
            None,
            "2030-01-01T00:00:03Z",
        ),
    );

    publish_rejected(
        &store,
        &question_op(
            "bob",
            &question_id,
            DecisionQuestionAction::Answer,
            &opened,
            vec![DiscussionReference::Post {
                post_id: wrong_topic,
            }],
            None,
            None,
            "wrong-topic-answer",
            "2030-01-01T00:00:04Z",
        ),
        "same-topic post citation",
    );
    let answer_op = question_op(
        "bob",
        &question_id,
        DecisionQuestionAction::Answer,
        &opened,
        vec![DiscussionReference::Post {
            post_id: answer.clone(),
        }],
        Some("candidate, not closure"),
        None,
        "answer-1",
        "2030-01-01T00:00:05Z",
    );
    let answered = publish_checked(&store, &answer_op);
    let after_answer = reducer::replay_store(&store).unwrap();
    let question = &after_answer.board_questions[&question_id];
    assert_eq!(question.status, DecisionQuestionStatus::Open);
    assert_eq!(question.clock_op_id, answered);
    assert_eq!(question.transitions.len(), 1);
    assert_eq!(
        question.transitions[0].action,
        DecisionQuestionAction::Answer
    );

    publish_rejected(
        &store,
        &question_op(
            "bob",
            &question_id,
            DecisionQuestionAction::Defer,
            &answered,
            Vec::new(),
            Some("later"),
            None,
            "unauthorized-defer",
            "2030-01-01T00:00:06Z",
        ),
        "decision author",
    );
    let deferred = publish_checked(
        &store,
        &question_op(
            "alice",
            &question_id,
            DecisionQuestionAction::Defer,
            &answered,
            Vec::new(),
            Some("after the prototype"),
            None,
            "defer-1",
            "2030-01-01T00:00:07Z",
        ),
    );
    publish_rejected(
        &store,
        &question_op(
            "bob",
            &question_id,
            DecisionQuestionAction::Answer,
            &deferred,
            vec![DiscussionReference::Post { post_id: answer }],
            None,
            None,
            "answer-after-defer",
            "2030-01-01T00:00:08Z",
        ),
        "open question",
    );
    let closed = publish_checked(
        &store,
        &question_op(
            "alice",
            &question_id,
            DecisionQuestionAction::Close,
            &deferred,
            Vec::new(),
            Some("prototype established the boundary"),
            None,
            "close-1",
            "2030-01-01T00:00:09Z",
        ),
    );
    publish_rejected(
        &store,
        &question_op(
            "alice",
            &question_id,
            DecisionQuestionAction::Close,
            &closed,
            Vec::new(),
            Some("close again"),
            None,
            "close-2",
            "2030-01-01T00:00:10Z",
        ),
        "unresolved question",
    );

    let state = reducer::replay_store(&store).unwrap();
    let question = &state.board_questions[&question_id];
    assert_eq!(question.status, DecisionQuestionStatus::Closed);
    assert_eq!(question.transitions.len(), 3);
    let event_types = accepted_events(
        &store,
        None,
        &EventFilter::new(&["discussion".into()], None).unwrap(),
    )
    .unwrap()
    .into_iter()
    .map(|event| event.event_type)
    .collect::<Vec<_>>();
    assert!(
        event_types.contains(&"discussion.question_answered".to_string())
            && event_types.contains(&"discussion.question_deferred".to_string())
            && event_types.contains(&"discussion.question_closed".to_string())
    );
}

#[test]
fn supersession_cas_and_actor_scoped_idempotency_fail_closed() {
    let (_temp, store) = setup();
    let agreed = post(
        &store,
        "alice",
        "architecture",
        "agreed clause",
        "2030-01-01T00:00:00Z",
    );
    let decision_id = ids::new_post_id();
    let first_question = ids::new_question_id();
    let successor = ids::new_question_id();
    let opened = publish_checked(
        &store,
        &decision(
            "alice",
            &decision_id,
            "architecture",
            vec![agreed],
            Vec::new(),
            vec![
                (&first_question, "Old formulation?"),
                (&successor, "Precise formulation?"),
            ],
            Vec::new(),
            None,
            "2030-01-01T00:00:01Z",
        ),
    );
    let supersede = question_op(
        "alice",
        &first_question,
        DecisionQuestionAction::Supersede,
        &opened,
        Vec::new(),
        Some("use the precise question"),
        Some(&successor),
        "supersede-1",
        "2030-01-01T00:00:02Z",
    );
    let superseded = publish_checked(&store, &supersede);

    // A byte-for-byte semantic retry is recorded as a duplicate intent without
    // adding state or a second accepted event, even though its compare clock is
    // now old. The CLI maps this back to the original successful result.
    publish_rejected(&store, &supersede, "idempotent retry already accepted");
    let state = reducer::replay_store(&store).unwrap();
    let first = &state.board_questions[&first_question];
    assert_eq!(first.status, DecisionQuestionStatus::Superseded);
    assert_eq!(first.clock_op_id, superseded);
    assert_eq!(
        first.successor_question_id.as_deref(),
        Some(successor.as_str())
    );
    assert_eq!(first.transitions.len(), 1);
    let supersession_events = accepted_events(
        &store,
        None,
        &EventFilter::new(&["discussion".into()], None).unwrap(),
    )
    .unwrap()
    .into_iter()
    .filter(|event| event.event_type == "discussion.question_superseded")
    .count();
    assert_eq!(supersession_events, 1);

    publish_rejected(
        &store,
        &question_op(
            "alice",
            &successor,
            DecisionQuestionAction::Close,
            &opened,
            Vec::new(),
            Some("different use"),
            None,
            "supersede-1",
            "2030-01-01T00:00:03Z",
        ),
        "already used",
    );

    let first_close = question_op(
        "alice",
        &successor,
        DecisionQuestionAction::Close,
        &opened,
        Vec::new(),
        Some("winner"),
        None,
        "successor-close-1",
        "2030-01-01T00:00:04Z",
    );
    let stale_close = question_op(
        "alice",
        &successor,
        DecisionQuestionAction::Close,
        &opened,
        Vec::new(),
        Some("loser"),
        None,
        "successor-close-2",
        "2030-01-01T00:00:04Z",
    );
    publish_checked(&store, &first_close);
    publish_rejected(&store, &stale_close, "stale question CAS");

    let state_one = reducer::replay_store(&store).unwrap();
    let state_two = reducer::replay_store(&store).unwrap();
    let first_projection =
        serde_json::to_value(&state_one.board_questions[&first_question]).unwrap();
    let second_projection =
        serde_json::to_value(&state_two.board_questions[&first_question]).unwrap();
    assert_eq!(first_projection, second_projection);
    assert_eq!(
        state_one.board_questions[&successor].status,
        DecisionQuestionStatus::Closed
    );
}

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_cli_store(temp: &TempDir) -> Store {
    let output = Command::new(mote_bin())
        .arg("init")
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Store::open(&temp.path().join(".mote")).unwrap()
}

fn run_full(temp: &TempDir, actor: &str, args: &[&str]) -> Output {
    Command::new(mote_bin())
        .args(args)
        .args(["--actor", actor])
        .current_dir(temp.path())
        .output()
        .unwrap()
}

fn run(temp: &TempDir, actor: &str, args: &[&str]) -> String {
    let output = run_full(temp, actor, args);
    assert!(
        output.status.success(),
        "`mote {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn cli_creates_shows_answers_and_closes_without_inference() {
    let temp = TempDir::new().unwrap();
    let store = init_cli_store(&temp);
    let agreed = run(
        &temp,
        "alice",
        &[
            "discuss",
            "post",
            "--topic",
            "architecture",
            "--body",
            "Use a text rail for exact relationships.",
        ],
    );
    let issue_id = run(&temp, "alice", &["new", "decision follow-up"]);
    let decide_args = [
        "--json",
        "discuss",
        "decide",
        "--topic",
        "architecture",
        "--agreed",
        agreed.as_str(),
        "--open",
        "Does the text rail carry cross-page relations?",
        "--issue",
        issue_id.as_str(),
        "--cite",
        "url:https://example.test/mockup",
        "--body",
        "Adopt the cited text-rail contract.",
        "--idempotency-key",
        "cli-decision-1",
    ];
    let created: serde_json::Value =
        serde_json::from_str(&run(&temp, "alice", &decide_args)).unwrap();
    assert_eq!(created["idempotent_retry"], false);
    assert_eq!(created["agreed_post_ids"][0], agreed);
    assert_eq!(created["resolved_references"][0]["disposition"], "active");
    let decision_id = created["decision_id"].as_str().unwrap().to_string();
    let question_id = created["questions"][0]["question_id"]
        .as_str()
        .unwrap()
        .to_string();
    let opened = created["questions"][0]["clock_op_id"]
        .as_str()
        .unwrap()
        .to_string();

    let retry: serde_json::Value =
        serde_json::from_str(&run(&temp, "alice", &decide_args)).unwrap();
    assert_eq!(retry["decision_id"], decision_id);
    assert_eq!(retry["idempotent_retry"], true);

    let answer_post = run(
        &temp,
        "bob",
        &[
            "discuss",
            "post",
            "--topic",
            "architecture",
            "--body",
            "It carries exact relations by identifier.",
        ],
    );
    let answered: serde_json::Value = serde_json::from_str(&run(
        &temp,
        "bob",
        &[
            "--json",
            "discuss",
            "question",
            "answer",
            question_id.as_str(),
            "--post",
            answer_post.as_str(),
            "--expect",
            opened.as_str(),
            "--idempotency-key",
            "cli-answer-1",
        ],
    ))
    .unwrap();
    assert_eq!(answered["status"], "open");
    assert_eq!(answered["answer_count"], 1);
    let answer_clock = answered["clock_op_id"].as_str().unwrap();

    let closed: serde_json::Value = serde_json::from_str(&run(
        &temp,
        "alice",
        &[
            "--json",
            "discuss",
            "question",
            "close",
            question_id.as_str(),
            "--resolution",
            "The cited answer defines the relationship rail.",
            "--cite",
            &format!("post:{answer_post}"),
            "--expect",
            answer_clock,
            "--idempotency-key",
            "cli-close-1",
        ],
    ))
    .unwrap();
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["unresolved"], false);

    let shown: serde_json::Value = serde_json::from_str(&run(
        &temp,
        "nobody",
        &[
            "--json",
            "discuss",
            "decide",
            "--topic",
            "architecture",
            "--show",
        ],
    ))
    .unwrap();
    assert_eq!(shown["structured_decision_count"], 1);
    assert_eq!(shown["unresolved_question_count"], 0);
    assert_eq!(shown["decisions"][0]["decision_id"], decision_id);
    assert_eq!(shown["decisions"][0]["questions"][0]["answer_count"], 1);

    let topics: serde_json::Value =
        serde_json::from_str(&run(&temp, "nobody", &["--json", "discuss", "topics"])).unwrap();
    let topic = topics
        .as_array()
        .unwrap()
        .iter()
        .find(|topic| topic["topic"] == "architecture")
        .unwrap();
    assert_eq!(topic["structured_decision_count"], 1);
    assert_eq!(topic["closed_question_count"], 1);
    assert_eq!(topic["unresolved_question_count"], 0);

    let posts: serde_json::Value = serde_json::from_str(&run(
        &temp,
        "nobody",
        &["--json", "discuss", "list", "--topic", "architecture"],
    ))
    .unwrap();
    let decision_post = posts
        .as_array()
        .unwrap()
        .iter()
        .find(|post| post["post_id"] == decision_id)
        .unwrap();
    assert_eq!(
        decision_post["decision"]["questions"][0]["status"],
        "closed"
    );

    let board: serde_json::Value =
        serde_json::from_str(&run(&temp, "nobody", &["--json", "board"])).unwrap();
    assert_eq!(board["discussion"]["structured_decision_count"], 1);
    assert_eq!(board["discussion"]["unresolved_question_count"], 0);

    let summary: serde_json::Value = serde_json::from_str(&run(
        &temp,
        "nobody",
        &["--json", "discuss", "summary", "--topic", "architecture"],
    ))
    .unwrap();
    assert_eq!(summary["discussion"]["structured_decision_count"], 1);
    assert_eq!(
        summary["discussion"]["decisions"][0]["decision_id"],
        decision_id
    );

    let human = run(
        &temp,
        "nobody",
        &["discuss", "decide", "--topic", "architecture", "--show"],
    );
    assert!(human.contains("AGREED — cited posts:"), "{human}");
    assert!(human.contains("QUESTION [CLOSED]"), "{human}");

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.board_decisions.len(), 1);
    assert_eq!(state.board_questions.len(), 1);
    // The CLI retry is handled before publish, so the log has one transition
    // for each caller action and no hidden retry operation.
    assert_eq!(state.board_questions[&question_id].transitions.len(), 2);
}

#[test]
fn operation_discriminators_are_fail_closed_protocol_boundaries() {
    let decision = decision(
        "alice",
        &ids::new_post_id(),
        "architecture",
        vec![ids::new_post_id()],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        "2030-01-01T00:00:00Z",
    );
    let question = question_op(
        "alice",
        &ids::new_question_id(),
        DecisionQuestionAction::Close,
        "op-clock",
        Vec::new(),
        Some("resolved"),
        None,
        "close-key",
        "2030-01-01T00:00:01Z",
    );
    assert_eq!(
        serde_json::to_value(decision).unwrap()["kind"],
        "board_decision"
    );
    assert_eq!(
        serde_json::to_value(question).unwrap()["kind"],
        "board_question"
    );
    assert!(
        serde_json::from_value::<Op>(serde_json::json!({
            "kind": "board_decision_v2",
            "v": 2,
            "op": "op-new",
            "ts": "2030-01-01T00:00:00Z",
            "actor": "alice"
        }))
        .is_err()
    );
}
