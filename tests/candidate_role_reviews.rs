use std::path::Path;
use std::process::Command;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::actor_status;
use mote::audit::{AuditFailOn, run_audit};
use mote::candidate::{
    CANDIDATE_PORTABLE_PROTOCOL_VERSION, CANDIDATE_PROTOCOL_VERSION, CANDIDATE_ROLE_REVIEW_VERSION,
    CandidateReviewPolicy, CandidateRoleReviewBinding, EvidenceRequirement, ReviewVerdict,
    RoleReviewRequirement,
};
use mote::events::{EventFilter, accepted_events};
use mote::ids;
use mote::op::{
    CandidateProposeOp, CandidateReviewOp, CandidateSupersedeOp, Op, RoleAssignOp, RoleDefineOp,
    RoleReleaseOp, RoleRenewOp, ScalarSet, make_create, make_reserve_close, make_reserve_open,
    make_session_start,
};
use mote::role::{ROLE_PROTOCOL_VERSION, RoleExclusion, RoleExclusionCode};
use mote::{publish, reducer, repo::Store};

const BASE: &str = "1111111111111111111111111111111111111111";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn timestamp(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn setup() -> (TempDir, Store, String) {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish_checked(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("role review candidate".into()),
                ..Default::default()
            },
            timestamp("2030-01-01T00:00:00Z"),
        ),
    );
    (temp, store, issue)
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

fn define_role(
    store: &Store,
    name: &str,
    capacity: u32,
    exclusions: Vec<RoleExclusion>,
    at: &str,
) -> (String, String) {
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        store,
        &Op::RoleDefine(RoleDefineOp {
            v: ROLE_PROTOCOL_VERSION,
            op: String::new(),
            ts: at.into(),
            actor: "chief".into(),
            role_id: role_id.clone(),
            name: name.into(),
            remit: format!("{name} remit"),
            exclusions,
            assignment_authorities: vec!["chief".into()],
            capacity,
            minimum_active: 0,
            idempotency_key: format!("define-{name}"),
        }),
    );
    (role_id, definition)
}

fn start_session(store: &Store, actor: &str, at: &str, ttl_s: u32) -> (String, String) {
    let session_id = ids::new_session_id();
    let operation = make_session_start(
        actor.into(),
        session_id.clone(),
        ttl_s,
        None,
        None,
        timestamp(at),
    );
    let op_id = publish_checked(store, &operation);
    (session_id, op_id)
}

#[allow(clippy::too_many_arguments)]
fn assign_role(
    store: &Store,
    role_id: &str,
    definition_op_id: &str,
    actor: &str,
    session_id: &str,
    session_op_id: &str,
    ttl_s: u32,
    at: &str,
    key: &str,
) -> (String, String) {
    let assignment_id = ids::new_role_assignment_id();
    let state = reducer::replay_store(store).unwrap();
    let op_id = publish_checked(
        store,
        &Op::RoleAssign(RoleAssignOp {
            v: ROLE_PROTOCOL_VERSION,
            op: String::new(),
            ts: at.into(),
            actor: "chief".into(),
            role_id: role_id.into(),
            expect_definition: definition_op_id.into(),
            assignment_id: assignment_id.clone(),
            holder_actor: actor.into(),
            holder_session_id: session_id.into(),
            expect_session: session_op_id.into(),
            ttl_s,
            expect_active: state.role_active_assignment_clocks(role_id, at),
            idempotency_key: key.into(),
        }),
    );
    (assignment_id, op_id)
}

#[allow(clippy::too_many_arguments)]
fn role_candidate(
    store: &Store,
    candidate_id: &str,
    issue: &str,
    commit: &str,
    authorizer: &str,
    named_reviewers: Vec<&str>,
    role_requirements: Vec<(&str, &str, u32)>,
    at: &str,
    key: &str,
) -> Op {
    Op::CandidatePropose(CandidateProposeOp {
        v: CANDIDATE_ROLE_REVIEW_VERSION,
        op: String::new(),
        ts: at.into(),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        entity: issue.into(),
        store_id: store.read_format().unwrap().store_id,
        repository_id: "repo-test".into(),
        landing_repository_id: None,
        object_source: None,
        object_format: "sha1".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        paths: vec!["src/lib.rs".into()],
        authorizer: authorizer.into(),
        reviewers: Vec::new(),
        review_policy: Some(CandidateReviewPolicy {
            named_reviewers: named_reviewers.into_iter().map(str::to_string).collect(),
            role_requirements: role_requirements
                .into_iter()
                .map(
                    |(role_id, definition_op_id, required_approvals)| RoleReviewRequirement {
                        role_id: role_id.into(),
                        definition_op_id: definition_op_id.into(),
                        required_approvals,
                    },
                )
                .collect(),
        }),
        evidence_requirements: vec![EvidenceRequirement {
            name: "git-ancestry".into(),
            kind: "git".into(),
            producers: vec!["proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

fn named_candidate(
    store: &Store,
    candidate_id: &str,
    issue: &str,
    commit: &str,
    at: &str,
    key: &str,
) -> Op {
    Op::CandidatePropose(CandidateProposeOp {
        v: CANDIDATE_PROTOCOL_VERSION,
        op: String::new(),
        ts: at.into(),
        actor: "successor-proposer".into(),
        candidate_id: candidate_id.into(),
        entity: issue.into(),
        store_id: store.read_format().unwrap().store_id,
        repository_id: "repo-test".into(),
        landing_repository_id: None,
        object_source: None,
        object_format: "sha1".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        paths: vec!["src/lib.rs".into()],
        authorizer: "successor-authorizer".into(),
        reviewers: vec!["successor-reviewer".into()],
        review_policy: None,
        evidence_requirements: vec![EvidenceRequirement {
            name: "git-ancestry".into(),
            kind: "git".into(),
            producers: vec!["successor-proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn role_review(
    candidate_id: &str,
    actor: &str,
    role_id: &str,
    assignment_id: &str,
    assignment_clock: &str,
    verdict: ReviewVerdict,
    expect_review: Option<&str>,
    at: &str,
    key: &str,
) -> Op {
    Op::CandidateReview(CandidateReviewOp {
        v: CANDIDATE_ROLE_REVIEW_VERSION,
        op: String::new(),
        ts: at.into(),
        actor: actor.into(),
        candidate_id: candidate_id.into(),
        verdict,
        body: None,
        evidence_refs: Vec::new(),
        expect_review: expect_review.map(str::to_string),
        role: Some(CandidateRoleReviewBinding {
            role_id: role_id.into(),
            assignment_id: assignment_id.into(),
            expect_assignment: assignment_clock.into(),
        }),
        idempotency_key: key.into(),
    })
}

fn named_review(
    candidate_id: &str,
    actor: &str,
    verdict: ReviewVerdict,
    at: &str,
    key: &str,
) -> Op {
    Op::CandidateReview(CandidateReviewOp {
        v: CANDIDATE_PROTOCOL_VERSION,
        op: String::new(),
        ts: at.into(),
        actor: actor.into(),
        candidate_id: candidate_id.into(),
        verdict,
        body: None,
        evidence_refs: Vec::new(),
        expect_review: None,
        role: None,
        idempotency_key: key.into(),
    })
}

fn release_assignment(
    store: &Store,
    role_id: &str,
    assignment_id: &str,
    assignment_clock: &str,
    at: &str,
    key: &str,
) -> String {
    publish_checked(
        store,
        &Op::RoleRelease(RoleReleaseOp {
            v: ROLE_PROTOCOL_VERSION,
            op: String::new(),
            ts: at.into(),
            actor: "chief".into(),
            role_id: role_id.into(),
            assignment_id: assignment_id.into(),
            expect_assignment: assignment_clock.into(),
            reason: Some("reassigned".into()),
            idempotency_key: key.into(),
        }),
    )
}

fn run_mote(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn legacy_policy_stays_compatible_and_role_policy_wire_fails_closed() {
    let (_temp, store, issue) = setup();
    let legacy_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &named_candidate(
            &store,
            &legacy_id,
            &issue,
            COMMIT_A,
            "2030-01-01T00:00:01Z",
            "legacy-policy",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.candidates[&legacy_id].review_policy_version, 1);
    assert!(
        state.candidates[&legacy_id]
            .role_review_requirements
            .is_empty()
    );

    let (role_id, definition) =
        define_role(&store, "reviewer", 2, Vec::new(), "2030-01-01T00:00:02Z");
    let candidate_id = ids::new_candidate_id();
    let proposal = role_candidate(
        &store,
        &candidate_id,
        &issue,
        COMMIT_B,
        "authorizer",
        Vec::new(),
        vec![(&role_id, &definition, 2)],
        "2030-01-01T00:00:03Z",
        "role-policy",
    );
    let value = serde_json::to_value(&proposal).unwrap();
    assert_eq!(value["v"], CANDIDATE_ROLE_REVIEW_VERSION);
    assert_eq!(value["reviewers"], serde_json::json!([]));
    assert_eq!(
        value["review_policy"]["role_requirements"][0]["role_id"],
        role_id
    );
    publish_checked(&store, &proposal);

    let malformed_id = ids::new_candidate_id();
    let mut malformed = role_candidate(
        &store,
        &malformed_id,
        &issue,
        "cccccccccccccccccccccccccccccccccccccccc",
        "authorizer",
        Vec::new(),
        vec![(&role_id, &definition, 1)],
        "2030-01-01T00:00:04Z",
        "malformed-role-policy",
    );
    let Op::CandidatePropose(malformed) = &mut malformed else {
        unreachable!()
    };
    malformed.review_policy = None;
    publish_rejected(
        &store,
        &Op::CandidatePropose(malformed.clone()),
        "unsupported or inconsistent candidate review policy version",
    );

    let impossible_id = ids::new_candidate_id();
    publish_rejected(
        &store,
        &role_candidate(
            &store,
            &impossible_id,
            &issue,
            "dddddddddddddddddddddddddddddddddddddddd",
            "authorizer",
            Vec::new(),
            vec![(&role_id, &definition, 3)],
            "2030-01-01T00:00:05Z",
            "impossible-role-policy",
        ),
        "have capacity for 3 distinct approval",
    );
}

#[test]
fn quorum_counts_distinct_bound_reviews_and_expiry_reopens_the_gap() {
    let (temp, store, issue) = setup();
    let (role_id, definition) =
        define_role(&store, "reviewer", 2, Vec::new(), "2030-01-01T00:00:01Z");
    let (alice_session, alice_session_op) =
        start_session(&store, "alice", "2030-01-01T00:00:02Z", 1_000);
    let (alice_assignment, alice_assignment_op) = assign_role(
        &store,
        &role_id,
        &definition,
        "alice",
        &alice_session,
        &alice_session_op,
        50,
        "2030-01-01T00:00:03Z",
        "assign-alice",
    );
    let (bob_session, bob_session_op) = start_session(&store, "bob", "2030-01-01T00:00:04Z", 1_000);
    let (bob_assignment, bob_assignment_op) = assign_role(
        &store,
        &role_id,
        &definition,
        "bob",
        &bob_session,
        &bob_session_op,
        200,
        "2030-01-01T00:00:05Z",
        "assign-bob",
    );
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &role_candidate(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            "authorizer",
            Vec::new(),
            vec![(&role_id, &definition, 2)],
            "2030-01-01T00:00:10Z",
            "quorum-candidate",
        ),
    );

    let initial = reducer::replay_store(&store).unwrap();
    let initial_coverage = initial
        .role_coverage(&role_id, "2030-01-01T00:00:11Z")
        .unwrap();
    assert_eq!(initial_coverage.demanded_count, 2);
    assert_eq!(initial_coverage.active_count, 2);
    assert_eq!(initial_coverage.coverage_shortfall, 0);
    let alice_status = actor_status::actor_status(
        &initial,
        "alice",
        None,
        timestamp("2030-01-01T00:00:11Z"),
        600,
    );
    assert!(
        alice_status.work.candidates[0]
            .roles
            .contains(&"role_reviewer:reviewer".to_string())
    );
    let candidate_events = accepted_events(
        &store,
        None,
        &EventFilter::new(&["candidate".into()], Some("alice".into())).unwrap(),
    )
    .unwrap();
    assert!(candidate_events.iter().any(|event| {
        event.event_type == "candidate.proposed"
            && event.data["candidate_id"].as_str() == Some(candidate_id.as_str())
    }));

    let alice_review = publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "alice",
            &role_id,
            &alice_assignment,
            &alice_assignment_op,
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:12Z",
            "alice-approve-1",
        ),
    );
    publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "alice",
            &role_id,
            &alice_assignment,
            &alice_assignment_op,
            ReviewVerdict::Approve,
            Some(&alice_review),
            "2030-01-01T00:00:13Z",
            "alice-approve-2",
        ),
    );
    let one = reducer::replay_store(&store).unwrap();
    assert_eq!(
        one.candidate_review_status(&candidate_id, "2030-01-01T00:00:13Z")
            .unwrap()
            .roles[0]
            .eligible_approval_count,
        1
    );

    publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "bob",
            &role_id,
            &bob_assignment,
            &bob_assignment_op,
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:14Z",
            "bob-approve",
        ),
    );
    let complete = reducer::replay_store(&store).unwrap();
    let complete_status = complete
        .candidate_review_status(&candidate_id, "2030-01-01T00:00:15Z")
        .unwrap();
    assert!(complete_status.satisfied);
    let before_reviews = complete
        .candidate_review_status(&candidate_id, "2030-01-01T00:00:11Z")
        .unwrap();
    assert_eq!(before_reviews.roles[0].eligible_approval_count, 0);
    assert!(before_reviews.roles[0].reviews.is_empty());
    assert_eq!(
        complete
            .role_coverage(&role_id, "2030-01-01T00:00:15Z")
            .unwrap()
            .demanded_count,
        0
    );
    assert!(
        !complete
            .candidate_landability_at(&candidate_id, None, "2030-01-01T00:00:15Z")
            .reason_codes
            .contains(&"review_role_quorum_missing".to_string())
    );

    let expired_status = complete
        .candidate_review_status(&candidate_id, "2030-01-01T00:00:54Z")
        .unwrap();
    assert!(!expired_status.satisfied);
    assert_eq!(expired_status.roles[0].eligible_approval_count, 1);
    assert_eq!(
        expired_status.roles[0].reviews[0].reason_code.as_deref(),
        Some("assignment_inactive")
    );
    let expired_landability =
        complete.candidate_landability_at(&candidate_id, None, "2030-01-01T00:00:54Z");
    assert!(
        expired_landability
            .reason_codes
            .contains(&"review_role_quorum_missing".to_string())
    );
    assert!(
        expired_landability
            .reason_codes
            .contains(&"review_role_approval_ineligible".to_string())
    );
    let coverage = complete
        .role_coverage(&role_id, "2030-01-01T00:00:54Z")
        .unwrap();
    assert_eq!(coverage.active_count, 1);
    assert_eq!(coverage.demanded_count, 2);
    assert_eq!(coverage.coverage_shortfall, 1);
    let audit = run_audit(
        &store,
        &complete,
        temp.path(),
        timestamp("2030-01-01T00:00:54Z"),
        None,
        None,
        AuditFailOn::Never,
    )
    .unwrap();
    let finding = audit
        .findings
        .iter()
        .find(|finding| finding.code == "role_coverage_shortfall" && finding.subject == role_id)
        .unwrap();
    assert_eq!(
        finding.evidence["coverage"]["demand_sources"][0]["kind"],
        "candidate_review"
    );
    assert_eq!(
        finding.evidence["coverage"]["demand_sources"][0]["source_id"],
        candidate_id
    );
}

#[test]
fn named_and_role_slots_are_distinct_and_exclusions_are_rechecked() {
    let (_temp, store, issue) = setup();
    let (role_id, definition) = define_role(
        &store,
        "independent-reviewer",
        3,
        vec![RoleExclusion {
            code: RoleExclusionCode::CandidateNamedReviewer,
            target_role_id: None,
        }],
        "2030-01-01T00:00:01Z",
    );
    let mut assignments = Vec::new();
    for (actor, second) in [("alice", 2), ("carol", 4), ("proposer", 6)] {
        let at = format!("2030-01-01T00:00:{second:02}Z");
        let (session, session_op) = start_session(&store, actor, &at, 1_000);
        let assignment_at = format!("2030-01-01T00:00:{:02}Z", second + 1);
        let assignment = assign_role(
            &store,
            &role_id,
            &definition,
            actor,
            &session,
            &session_op,
            500,
            &assignment_at,
            &format!("assign-{actor}"),
        );
        assignments.push((actor, assignment));
    }
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &role_candidate(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            "authorizer",
            vec!["carol"],
            vec![(&role_id, &definition, 1)],
            "2030-01-01T00:00:10Z",
            "mixed-policy",
        ),
    );
    publish_checked(
        &store,
        &named_review(
            &candidate_id,
            "carol",
            ReviewVerdict::Approve,
            "2030-01-01T00:00:11Z",
            "carol-named",
        ),
    );
    publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "alice",
            &role_id,
            &assignments[0].1.0,
            &assignments[0].1.1,
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:12Z",
            "alice-role",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state
            .candidate_review_status(&candidate_id, "2030-01-01T00:00:13Z")
            .unwrap()
            .satisfied
    );
    let named_events = accepted_events(
        &store,
        None,
        &EventFilter::new(&["candidate".into()], Some("carol".into())).unwrap(),
    )
    .unwrap();
    assert!(named_events.iter().any(|event| {
        event.event_type == "candidate.proposed"
            && event.data["candidate_id"].as_str() == Some(candidate_id.as_str())
    }));

    publish_rejected(
        &store,
        &role_review(
            &candidate_id,
            "carol",
            &role_id,
            &assignments[1].1.0,
            &assignments[1].1.1,
            ReviewVerdict::Approve,
            state.reviews_clock(&candidate_id, "carol").as_deref(),
            "2030-01-01T00:00:14Z",
            "carol-role",
        ),
        "excluded_candidate_named_reviewer",
    );
    publish_rejected(
        &store,
        &role_review(
            &candidate_id,
            "proposer",
            &role_id,
            &assignments[2].1.0,
            &assignments[2].1.1,
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:15Z",
            "self-review",
        ),
        "cannot review their own candidate",
    );
}

#[test]
fn a_role_block_expires_with_its_assignment_and_reassignment_can_satisfy_quorum() {
    let (_temp, store, issue) = setup();
    let (role_id, definition) = define_role(
        &store,
        "release-reviewer",
        2,
        Vec::new(),
        "2030-01-01T00:00:01Z",
    );
    let (bob_session, bob_session_op) = start_session(&store, "bob", "2030-01-01T00:00:02Z", 1_000);
    let (bob_assignment, bob_assignment_op) = assign_role(
        &store,
        &role_id,
        &definition,
        "bob",
        &bob_session,
        &bob_session_op,
        500,
        "2030-01-01T00:00:03Z",
        "block-bob",
    );
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &role_candidate(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            "authorizer",
            Vec::new(),
            vec![(&role_id, &definition, 1)],
            "2030-01-01T00:00:04Z",
            "block-candidate",
        ),
    );
    publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "bob",
            &role_id,
            &bob_assignment,
            &bob_assignment_op,
            ReviewVerdict::Block,
            None,
            "2030-01-01T00:00:05Z",
            "bob-block",
        ),
    );
    let blocked = reducer::replay_store(&store).unwrap();
    assert!(
        blocked
            .candidate_landability_at(&candidate_id, None, "2030-01-01T00:00:06Z")
            .reason_codes
            .contains(&"review_role_blocking".to_string())
    );
    let renewed_assignment_op = publish_checked(
        &store,
        &Op::RoleRenew(RoleRenewOp {
            v: ROLE_PROTOCOL_VERSION,
            op: String::new(),
            ts: "2030-01-01T00:00:06Z".into(),
            actor: "chief".into(),
            role_id: role_id.clone(),
            assignment_id: bob_assignment.clone(),
            expect_assignment: bob_assignment_op,
            expect_session: bob_session_op,
            ttl_s: 500,
            idempotency_key: "renew-bob-block".into(),
        }),
    );
    assert_eq!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_review_status(&candidate_id, "2030-01-01T00:00:06Z")
            .unwrap()
            .roles[0]
            .eligible_block_count,
        1
    );
    release_assignment(
        &store,
        &role_id,
        &bob_assignment,
        &renewed_assignment_op,
        "2030-01-01T00:00:07Z",
        "release-bob",
    );
    let released = reducer::replay_store(&store).unwrap();
    let reasons = released
        .candidate_landability_at(&candidate_id, None, "2030-01-01T00:00:08Z")
        .reason_codes;
    assert!(!reasons.contains(&"review_role_blocking".to_string()));
    assert!(reasons.contains(&"review_role_quorum_missing".to_string()));

    let (alice_session, alice_session_op) =
        start_session(&store, "alice", "2030-01-01T00:00:09Z", 1_000);
    let (alice_assignment, alice_assignment_op) = assign_role(
        &store,
        &role_id,
        &definition,
        "alice",
        &alice_session,
        &alice_session_op,
        500,
        "2030-01-01T00:00:10Z",
        "replace-alice",
    );
    publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "alice",
            &role_id,
            &alice_assignment,
            &alice_assignment_op,
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:11Z",
            "alice-approve",
        ),
    );
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_review_status(&candidate_id, "2030-01-01T00:00:12Z")
            .unwrap()
            .satisfied
    );
}

#[test]
fn a_later_overlapping_reservation_makes_a_role_approval_ineligible() {
    let (_temp, store, issue) = setup();
    let (role_id, definition) = define_role(
        &store,
        "path-independent-reviewer",
        1,
        vec![RoleExclusion {
            code: RoleExclusionCode::CandidatePathReservation,
            target_role_id: None,
        }],
        "2030-01-01T00:00:01Z",
    );
    let (session, session_op) = start_session(&store, "alice", "2030-01-01T00:00:02Z", 1_000);
    let (assignment, assignment_op) = assign_role(
        &store,
        &role_id,
        &definition,
        "alice",
        &session,
        &session_op,
        500,
        "2030-01-01T00:00:03Z",
        "path-alice",
    );
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &role_candidate(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            "authorizer",
            Vec::new(),
            vec![(&role_id, &definition, 1)],
            "2030-01-01T00:00:04Z",
            "path-candidate",
        ),
    );
    publish_checked(
        &store,
        &role_review(
            &candidate_id,
            "alice",
            &role_id,
            &assignment,
            &assignment_op,
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:05Z",
            "path-approve",
        ),
    );
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_review_status(&candidate_id, "2030-01-01T00:00:06Z")
            .unwrap()
            .satisfied
    );

    let reservation_id = ids::new_reservation_id();
    publish_checked(
        &store,
        &make_reserve_open(
            "alice".into(),
            reservation_id.clone(),
            issue,
            vec!["src/lib.rs".into()],
            300,
            timestamp("2030-01-01T00:00:07Z"),
        ),
    );
    let conflicted = reducer::replay_store(&store).unwrap();
    let status = conflicted
        .candidate_review_status(&candidate_id, "2030-01-01T00:00:08Z")
        .unwrap();
    assert!(!status.satisfied);
    assert_eq!(
        status.roles[0].reviews[0].reason_code.as_deref(),
        Some("excluded_candidate_path_reservation")
    );

    publish_checked(
        &store,
        &make_reserve_close(
            "alice".into(),
            reservation_id,
            None,
            timestamp("2030-01-01T00:00:09Z"),
        ),
    );
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_review_status(&candidate_id, "2030-01-01T00:00:10Z")
            .unwrap()
            .satisfied
    );
}

#[test]
fn supersession_removes_contextual_role_demand_and_terminal_review_is_rejected() {
    let (_temp, store, issue) = setup();
    let (role_id, definition) = define_role(
        &store,
        "transient-reviewer",
        1,
        Vec::new(),
        "2030-01-01T00:00:01Z",
    );
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &role_candidate(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            "authorizer",
            Vec::new(),
            vec![(&role_id, &definition, 1)],
            "2030-01-01T00:00:02Z",
            "old-role-candidate",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state
            .role_coverage(&role_id, "2030-01-01T00:00:03Z")
            .unwrap()
            .demanded_count,
        1
    );

    let successor_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &named_candidate(
            &store,
            &successor_id,
            &issue,
            COMMIT_B,
            "2030-01-01T00:00:04Z",
            "successor",
        ),
    );
    publish_checked(
        &store,
        &Op::CandidateSupersede(CandidateSupersedeOp {
            v: CANDIDATE_PROTOCOL_VERSION,
            op: String::new(),
            ts: "2030-01-01T00:00:05Z".into(),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            successor_id,
            expect_phase: proposal_op,
            recovery: None,
            idempotency_key: "supersede-role-demand".into(),
        }),
    );
    let terminal = reducer::replay_store(&store).unwrap();
    assert_eq!(
        terminal
            .role_coverage(&role_id, "2030-01-01T00:00:06Z")
            .unwrap()
            .demanded_count,
        0
    );
    publish_rejected(
        &store,
        &role_review(
            &candidate_id,
            "alice",
            &role_id,
            &ids::new_role_assignment_id(),
            "missing-clock",
            ReviewVerdict::Approve,
            None,
            "2030-01-01T00:00:07Z",
            "review-terminal",
        ),
        "only a pending candidate may be reviewed",
    );
}

#[test]
fn cli_role_review_uses_exact_assignment_and_retries_without_appending() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());
    let issue = ids::new_bead_id();
    publish_checked(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("CLI role review".into()),
                ..Default::default()
            },
            timestamp(&now),
        ),
    );
    let (role_id, definition) = define_role(&store, "cli-reviewer", 1, Vec::new(), &now);
    let (session, session_op) = start_session(&store, "alice", &now, 7_200);
    let (_assignment, _assignment_op) = assign_role(
        &store,
        &role_id,
        &definition,
        "alice",
        &session,
        &session_op,
        3_600,
        &now,
        "cli-assign",
    );
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &role_candidate(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            "authorizer",
            Vec::new(),
            vec![(&role_id, &definition, 1)],
            &now,
            "cli-candidate",
        ),
    );
    let args = [
        "--json",
        "--actor",
        "alice",
        "candidate",
        "review",
        &candidate_id,
        "approve",
        "--from-role",
        "cli-reviewer",
        "--idempotency-key",
        "cli-role-review",
    ];
    let first = run_mote(temp.path(), &args);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_json: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(
        first_json["review_status"]["roles"][0]["eligible_approval_count"],
        1
    );
    assert_eq!(
        first_json["reviews"]["alice"]["qualification"]["kind"],
        "role_assignment"
    );
    let count = store.list_op_filenames().unwrap().len();
    let retry = run_mote(temp.path(), &args);
    assert!(retry.status.success());
    assert_eq!(store.list_op_filenames().unwrap().len(), count);
}

#[test]
fn cli_propose_accepts_the_paired_role_and_count_policy() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(temp.path(), &["config", "user.email", "test@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Test"]);
    std::fs::write(temp.path().join("work.txt"), "base\n").unwrap();
    run_git(temp.path(), &["add", "work.txt"]);
    run_git(temp.path(), &["commit", "-qm", "base"]);
    let base = run_git(temp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(temp.path().join("work.txt"), "candidate\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "candidate"]);

    let store = Store::init(temp.path()).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());
    let issue = ids::new_bead_id();
    publish_checked(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("CLI role proposal".into()),
                ..Default::default()
            },
            timestamp(&now),
        ),
    );
    let (role_id, definition) = define_role(&store, "proposal-reviewer", 1, Vec::new(), &now);
    let (session, session_op) = start_session(&store, "alice", &now, 7_200);
    assign_role(
        &store,
        &role_id,
        &definition,
        "alice",
        &session,
        &session_op,
        3_600,
        &now,
        "proposal-alice",
    );

    let proposed = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "proposer",
            "candidate",
            "propose",
            "--issue",
            &issue,
            "--base",
            &base,
            "--path",
            "work.txt",
            "--authorizer",
            "authorizer",
            "--require-reviews",
            "1",
            "--from-role",
            "proposal-reviewer",
            "--idempotency-key",
            "cli-role-proposal",
        ],
    );
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    assert_eq!(
        proposed["policy"]["review_policy_version"],
        CANDIDATE_PORTABLE_PROTOCOL_VERSION
    );
    assert_eq!(
        proposed["policy"]["role_review_requirements"][0]["role_id"],
        role_id
    );
    assert_eq!(
        proposed["policy"]["role_review_requirements"][0]["required_approvals"],
        1
    );
    assert!(
        proposed["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "review_role_quorum_missing")
    );

    let reviewed = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "alice",
            "candidate",
            "review",
            candidate_id,
            "approve",
            "--from-role",
            "proposal-reviewer",
            "--idempotency-key",
            "cli-role-proposal-review",
        ],
    );
    assert!(
        reviewed.status.success(),
        "{}",
        String::from_utf8_lossy(&reviewed.stderr)
    );
    let reviewed: serde_json::Value = serde_json::from_slice(&reviewed.stdout).unwrap();
    assert_eq!(reviewed["review_status"]["satisfied"], true);
}

trait CandidateReviewClock {
    fn reviews_clock(&self, candidate_id: &str, actor: &str) -> Option<String>;
}

impl CandidateReviewClock for mote::state::State {
    fn reviews_clock(&self, candidate_id: &str, actor: &str) -> Option<String> {
        self.candidates
            .get(candidate_id)?
            .reviews
            .get(actor)
            .map(|review| review.op_id.clone())
    }
}
