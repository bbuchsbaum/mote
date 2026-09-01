use std::process::{Command, Output};

use jiff::Timestamp;
use tempfile::TempDir;

use mote::candidate::{
    AuthorizationStatus, CandidateEvidencePayload, EvidenceOutcome, EvidenceRequirement,
    GIT_ANCESTRY_EVIDENCE, GitAncestryReceipt, ReviewVerdict,
};
use mote::ids;
use mote::op::{
    CandidateAuthorizeOp, CandidateEvidenceOp, CandidateProposeOp, CandidateReviewOp,
    CandidateReviewPolicyAmendOp, CandidateSupersedeOp, Op, ScalarSet, make_create,
};
use mote::{publish, reducer, repo::Store};

const BASE: &str = "1111111111111111111111111111111111111111";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const REPO: &str = "repo-test";

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
                title: Some("review policy amendment".into()),
                ..Default::default()
            },
            Timestamp::now(),
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

fn publish_rejected(store: &Store, operation: &Op, fragment: &str) {
    let name = publish::publish_op(store, operation).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    let reason = state.rejection_reason(name.as_str()).unwrap();
    assert!(
        reason.contains(fragment),
        "expected `{fragment}` in `{reason}`"
    );
}

fn proposal(
    store: &Store,
    candidate_id: &str,
    issue: &str,
    commit: &str,
    reviewers: Vec<&str>,
    key: &str,
) -> Op {
    Op::CandidatePropose(CandidateProposeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        entity: issue.into(),
        store_id: store.read_format().unwrap().store_id,
        repository_id: REPO.into(),
        landing_repository_id: None,
        object_source: None,
        object_format: "sha1".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        paths: vec!["src/lib.rs".into()],
        authorizer: "authorizer".into(),
        reviewers: reviewers.into_iter().map(str::to_string).collect(),
        review_policy: None,
        evidence_requirements: vec![EvidenceRequirement {
            name: GIT_ANCESTRY_EVIDENCE.into(),
            kind: "git".into(),
            producers: vec!["proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

fn ancestry(candidate_id: &str, commit: &str, key: &str) -> Op {
    let payload = CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        relation_schema: 1,
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        common_dir_hash: "common".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        base_is_ancestor: Some(true),
        candidate_relations: Vec::new(),
        covered_candidates: Vec::new(),
        producer_snapshot: None,
        git_version: "git version test".into(),
        detail: None,
    });
    Op::CandidateEvidence(CandidateEvidenceOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        candidate_oid: commit.into(),
        evidence_id: mote::candidate::evidence_id(&payload).unwrap(),
        name: GIT_ANCESTRY_EVIDENCE.into(),
        evidence_kind: "git".into(),
        producer_tool: "git version test".into(),
        outcome: EvidenceOutcome::Pass,
        payload,
        refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

fn review(candidate_id: &str, actor: &str, verdict: ReviewVerdict, key: &str) -> Op {
    Op::CandidateReview(CandidateReviewOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
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

fn authorize(candidate_id: &str, key: &str) -> Op {
    Op::CandidateAuthorize(CandidateAuthorizeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "authorizer".into(),
        candidate_id: candidate_id.into(),
        status: AuthorizationStatus::Granted,
        grantees: vec!["lander".into()],
        conditions: Vec::new(),
        expect_authorization: None,
        idempotency_key: key.into(),
    })
}

fn run_mote(temp: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(temp.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn authorizer_amends_named_reviewers_without_discarding_remaining_approval() {
    let (temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &proposal(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            vec!["alice", "bob"],
            "proposal",
        ),
    );
    publish_checked(&store, &ancestry(&candidate_id, COMMIT_A, "ancestry"));
    let alice_review = publish_checked(
        &store,
        &review(
            &candidate_id,
            "alice",
            ReviewVerdict::Approve,
            "alice-review",
        ),
    );
    publish_checked(&store, &authorize(&candidate_id, "authorize"));

    let output = run_mote(
        &temp,
        &[
            "--json",
            "--actor",
            "authorizer",
            "candidate",
            "amend-reviewers",
            &candidate_id,
            "--reviewer",
            "alice",
            "--reviewer",
            "carol",
            "--expect-phase",
            &proposal_op,
            "--expect-policy",
            &proposal_op,
            "--reason",
            "bob was reassigned; substitute carol",
            "--idempotency-key",
            "amend-reviewers",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        shown["policy"]["reviewers"],
        serde_json::json!(["alice", "carol"])
    );
    assert_eq!(
        shown["policy"]["amendments"][0]["removed_reviewers"][0],
        "bob"
    );
    assert_eq!(
        shown["policy"]["amendments"][0]["added_reviewers"][0],
        "carol"
    );

    let state = reducer::replay_store(&store).unwrap();
    let candidate = &state.candidates[&candidate_id];
    assert_eq!(candidate.reviews["alice"].op_id, alice_review);
    assert_eq!(candidate.review_policy_amendments.len(), 1);
    let landability = state.candidate_landability(&candidate_id, Some("lander"));
    assert!(landability.reasons.iter().any(|reason| {
        reason.code == "review_missing" && reason.subject.as_deref() == Some("carol")
    }));
    assert!(!landability.reasons.iter().any(|reason| {
        reason.code == "review_missing" && reason.subject.as_deref() == Some("bob")
    }));

    publish_checked(
        &store,
        &review(
            &candidate_id,
            "carol",
            ReviewVerdict::Approve,
            "carol-review",
        ),
    );
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_landability(&candidate_id, Some("lander"))
            .landable
    );
}

#[test]
fn amendment_is_authorizer_owned_cas_and_cannot_remove_every_requirement() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &proposal(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            vec!["alice", "bob"],
            "proposal-cas",
        ),
    );
    let amend = |actor: &str, reviewers: Vec<&str>, expect_policy: &str, key: &str| -> Op {
        Op::CandidateReviewPolicyAmend(CandidateReviewPolicyAmendOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: actor.into(),
            candidate_id: candidate_id.clone(),
            named_reviewers: reviewers.into_iter().map(str::to_string).collect(),
            expect_phase: proposal_op.clone(),
            expect_review_policy: expect_policy.into(),
            reason: "availability changed".into(),
            idempotency_key: key.into(),
        })
    };

    publish_rejected(
        &store,
        &amend(
            "proposer",
            vec!["alice", "carol"],
            &proposal_op,
            "wrong-actor",
        ),
        "proposal authorizer",
    );
    publish_rejected(
        &store,
        &amend("authorizer", Vec::new(), &proposal_op, "empty-policy"),
        "at least one review requirement",
    );
    let amendment_op = publish_checked(
        &store,
        &amend(
            "authorizer",
            vec!["alice", "carol"],
            &proposal_op,
            "first-amendment",
        ),
    );
    publish_rejected(
        &store,
        &amend(
            "authorizer",
            vec!["alice", "dana"],
            &proposal_op,
            "stale-policy",
        ),
        "current pending phase and policy CAS",
    );
    assert_eq!(
        reducer::replay_store(&store).unwrap().candidates[&candidate_id].review_policy_op_id,
        amendment_op
    );
}

#[test]
fn supersession_reports_every_review_that_does_not_carry() {
    let (temp, store, issue) = setup();
    let predecessor = ids::new_candidate_id();
    let predecessor_phase = publish_checked(
        &store,
        &proposal(
            &store,
            &predecessor,
            &issue,
            COMMIT_A,
            vec!["alice", "bob"],
            "predecessor",
        ),
    );
    publish_checked(
        &store,
        &review(
            &predecessor,
            "alice",
            ReviewVerdict::Approve,
            "alice-approval",
        ),
    );
    publish_checked(
        &store,
        &review(&predecessor, "bob", ReviewVerdict::Comment, "bob-comment"),
    );
    let successor = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(
            &store,
            &successor,
            &issue,
            COMMIT_B,
            vec!["alice", "bob"],
            "successor",
        ),
    );
    publish_checked(
        &store,
        &Op::CandidateSupersede(CandidateSupersedeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: predecessor.clone(),
            successor_id: successor.clone(),
            expect_phase: predecessor_phase,
            recovery: None,
            idempotency_key: "supersede".into(),
        }),
    );

    let state = reducer::replay_store(&store).unwrap();
    let not_carried = &state.candidates[&predecessor]
        .supersession
        .as_ref()
        .unwrap()
        .reviews_not_carried;
    assert_eq!(not_carried.len(), 2);
    assert_eq!(not_carried[0].reviewer, "alice");
    assert_eq!(not_carried[0].verdict, ReviewVerdict::Approve);
    assert!(state.candidates[&successor].reviews.is_empty());

    let shown = run_mote(&temp, &["candidate", "show", &predecessor]);
    assert!(shown.status.success());
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(shown.contains("reviews not carried: 2 total; 1 approval(s) [alice]"));
}

#[test]
fn removing_then_readding_a_reviewer_does_not_revive_their_old_approval() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let phase = publish_checked(
        &store,
        &proposal(
            &store,
            &candidate_id,
            &issue,
            COMMIT_A,
            vec!["alice", "bob"],
            "readd-proposal",
        ),
    );
    let old_review = publish_checked(
        &store,
        &review(
            &candidate_id,
            "alice",
            ReviewVerdict::Approve,
            "old-alice-review",
        ),
    );
    let removed = publish_checked(
        &store,
        &Op::CandidateReviewPolicyAmend(CandidateReviewPolicyAmendOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.clone(),
            named_reviewers: vec!["bob".into(), "carol".into()],
            expect_phase: phase.clone(),
            expect_review_policy: phase.clone(),
            reason: "alice unavailable".into(),
            idempotency_key: "remove-alice".into(),
        }),
    );
    publish_checked(
        &store,
        &Op::CandidateReviewPolicyAmend(CandidateReviewPolicyAmendOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.clone(),
            named_reviewers: vec!["alice".into(), "bob".into(), "carol".into()],
            expect_phase: phase,
            expect_review_policy: removed,
            reason: "alice available again".into(),
            idempotency_key: "readd-alice".into(),
        }),
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.candidates[&candidate_id].reviews["alice"].op_id,
        old_review
    );
    let status = state
        .candidate_review_status(&candidate_id, &ids::format_rfc3339(Timestamp::now()))
        .unwrap();
    let alice = status
        .named
        .iter()
        .find(|status| status.reviewer == "alice")
        .unwrap();
    assert_eq!(alice.verdict, None);
    assert!(!alice.satisfied);

    publish_checked(
        &store,
        &Op::CandidateReview(CandidateReviewOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "alice".into(),
            candidate_id: candidate_id.clone(),
            verdict: ReviewVerdict::Approve,
            body: None,
            evidence_refs: Vec::new(),
            expect_review: Some(old_review),
            role: None,
            idempotency_key: "fresh-alice-review".into(),
        }),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state
            .candidate_review_status(&candidate_id, &ids::format_rfc3339(Timestamp::now()))
            .unwrap()
            .named
            .iter()
            .find(|status| status.reviewer == "alice")
            .unwrap()
            .satisfied
    );
}
