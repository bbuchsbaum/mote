use jiff::Timestamp;
use std::process::Command;
use tempfile::TempDir;

use mote::candidate::{
    AuthorizationStatus, CandidateEvidencePayload, EvidenceOutcome, EvidenceRequirement,
    GIT_ANCESTRY_EVIDENCE, GIT_LANDING_EVIDENCE, GitAncestryReceipt, GitCandidateRelation,
    GitLandingReceipt, GitRelationKind, KnownCandidate, ReviewVerdict,
};
use mote::ids;
use mote::op::{
    CandidateAbandonOp, CandidateAuthorizeOp, CandidateEvidenceOp, CandidateLandedOp,
    CandidateProposeOp, CandidateReviewOp, CandidateRevokeOp, CandidateSupersedeOp, Op, ScalarSet,
    make_create, make_reserve_close, make_reserve_open,
};
use mote::state::LeaseDisposition;
use mote::{publish, reducer, repo::Store};

const BASE: &str = "1111111111111111111111111111111111111111";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const REPO: &str = "repo-test";

fn setup() -> (TempDir, Store, String) {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("candidate test".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    (temp, store, issue)
}

fn publish_checked(store: &Store, op: &Op) -> String {
    let name = publish::publish_op(store, op).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(
        state.was_accepted(name.as_str()),
        "rejected: {:?}",
        state.rejection_reason(name.as_str())
    );
    name.into_string()
}

fn reserve_candidate(store: &Store, candidate_id: &str, actor: &str) -> String {
    let reservation_id = ids::new_reservation_id();
    publish_checked(
        store,
        &make_reserve_open(
            actor.into(),
            reservation_id.clone(),
            candidate_id.into(),
            vec!["src/lib.rs".into()],
            3600,
            Timestamp::now(),
        ),
    );
    reservation_id
}

fn proposal(candidate_id: &str, issue: &str, commit: &str, key: &str) -> Op {
    Op::CandidatePropose(CandidateProposeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        entity: issue.into(),
        store_id: "st-test".into(),
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        paths: vec!["src/lib.rs".into()],
        authorizer: "authorizer".into(),
        reviewers: vec!["reviewer".into()],
        evidence_requirements: vec![EvidenceRequirement {
            name: GIT_ANCESTRY_EVIDENCE.into(),
            kind: "git".into(),
            producers: vec!["proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

fn ancestry_payload(
    candidate_id: &str,
    commit: &str,
    covered: Vec<(String, String)>,
    relations: Vec<GitCandidateRelation>,
) -> CandidateEvidencePayload {
    let _ = candidate_id;
    CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        common_dir_hash: "common".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        base_is_ancestor: Some(true),
        candidate_relations: relations,
        covered_candidates: covered,
        git_version: "git version test".into(),
        detail: None,
    })
}

fn evidence(candidate_id: &str, payload: CandidateEvidencePayload, key: &str) -> Op {
    Op::CandidateEvidence(CandidateEvidenceOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        candidate_oid: match &payload {
            CandidateEvidencePayload::GitAncestry(git) => git.commit_oid.clone(),
            _ => unreachable!(),
        },
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

fn approve(candidate_id: &str, key: &str) -> Op {
    Op::CandidateReview(CandidateReviewOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "reviewer".into(),
        candidate_id: candidate_id.into(),
        verdict: ReviewVerdict::Approve,
        body: None,
        evidence_refs: Vec::new(),
        expect_review: None,
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

fn abandon(store: &Store, candidate_id: &str, key: &str) {
    let state = reducer::replay_store(store).unwrap();
    publish_checked(
        store,
        &Op::CandidateAbandon(CandidateAbandonOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.into(),
            expect_phase: state.candidates[candidate_id].phase_op_id.clone(),
            reason: Some("did not govern the landing".into()),
            idempotency_key: key.into(),
        }),
    );
}

#[test]
fn happy_path_consumes_authorization_and_is_replay_deterministic() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(&candidate_id, &issue, COMMIT_A, "propose-a"),
    );
    let reservation_id = reserve_candidate(&store, &candidate_id, "proposer");
    publish_checked(
        &store,
        &evidence(
            &candidate_id,
            ancestry_payload(&candidate_id, COMMIT_A, Vec::new(), Vec::new()),
            "ancestry-a",
        ),
    );
    publish_checked(&store, &approve(&candidate_id, "review-a"));
    let authorization_op = publish_checked(&store, &authorize(&candidate_id, "authorize-a"));

    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state
            .candidate_landability(&candidate_id, Some("lander"))
            .landable
    );
    assert!(
        state
            .candidate_landability(&candidate_id, Some("intruder"))
            .reason_codes
            .contains(&"actor_not_grantee".into())
    );

    let landing_payload = CandidateEvidencePayload::GitLanding(GitLandingReceipt {
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        candidate_oid: COMMIT_A.into(),
        target_ref: "refs/heads/main".into(),
        before_tip: Some(BASE.into()),
        after_tip: COMMIT_A.into(),
        candidate_reachable: Some(true),
        authorization_op_id: authorization_op.clone(),
        basis_op_ids: Vec::new(),
        git_version: "git version test".into(),
        detail: None,
    });
    let landing_evidence_id = mote::candidate::evidence_id(&landing_payload).unwrap();
    publish_checked(
        &store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "lander".into(),
            candidate_id: candidate_id.clone(),
            candidate_oid: COMMIT_A.into(),
            evidence_id: landing_evidence_id.clone(),
            name: GIT_LANDING_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload: landing_payload,
            refs: Vec::new(),
            idempotency_key: "landing-receipt-a".into(),
        }),
    );
    let phase_op = state.candidates[&candidate_id].phase_op_id.clone();
    publish_checked(
        &store,
        &Op::CandidateLanded(CandidateLandedOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "lander".into(),
            candidate_id: candidate_id.clone(),
            evidence_id: landing_evidence_id,
            expect_phase: phase_op,
            expect_authorization: authorization_op,
            target_ref: "refs/heads/main".into(),
            idempotency_key: "landed-a".into(),
        }),
    );
    let first = format!("{:?}", reducer::replay_store(&store).unwrap());
    let second = format!("{:?}", reducer::replay_store(&store).unwrap());
    assert_eq!(first, second);
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.candidates[&candidate_id].phase.as_str(), "landed");
    assert_eq!(
        state.candidates[&candidate_id]
            .authorization
            .as_ref()
            .unwrap()
            .status,
        AuthorizationStatus::Consumed
    );
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&reservation_id],
            &ids::format_rfc3339(Timestamp::now())
        ),
        LeaseDisposition::Orphaned
    );

    let stale_revoke = Op::CandidateRevoke(CandidateRevokeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "authorizer".into(),
        candidate_id: candidate_id.clone(),
        expect_authorization: state.candidates[&candidate_id]
            .landed
            .as_ref()
            .unwrap()
            .authorization_op_id
            .clone(),
        reason: None,
        idempotency_key: "late-revoke".into(),
    });
    let name = publish::publish_op(&store, &stale_revoke).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
}

#[test]
fn hidden_pending_ancestor_blocks_until_superseded_by_descendant() {
    let (_temp, store, issue) = setup();
    let old = ids::new_candidate_id();
    let new = ids::new_candidate_id();
    let old_proposal = publish_checked(&store, &proposal(&old, &issue, COMMIT_A, "propose-old"));
    let old_reservation = reserve_candidate(&store, &old, "proposer");
    publish_checked(
        &store,
        &evidence(
            &old,
            ancestry_payload(&old, COMMIT_A, Vec::new(), Vec::new()),
            "ancestry-old",
        ),
    );
    publish_checked(&store, &proposal(&new, &issue, COMMIT_B, "propose-new"));
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state
            .candidate_landability(&old, Some("lander"))
            .reason_codes
            .contains(&"git_evidence_stale".into())
    );
    publish_checked(
        &store,
        &evidence(
            &new,
            ancestry_payload(
                &new,
                COMMIT_B,
                vec![(old.clone(), old_proposal.clone())],
                vec![GitCandidateRelation {
                    candidate_id: old.clone(),
                    proposal_op_id: old_proposal,
                    commit_oid: COMMIT_A.into(),
                    base_relation: Some(GitRelationKind::NotAncestor),
                    relation: GitRelationKind::Ancestor,
                }],
            ),
            "ancestry-new",
        ),
    );
    publish_checked(&store, &approve(&new, "review-new"));
    publish_checked(&store, &authorize(&new, "authorize-new"));
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state
            .candidate_landability(&new, Some("lander"))
            .reason_codes
            .contains(&"ancestor_pending".into())
    );

    publish_checked(
        &store,
        &Op::CandidateSupersede(CandidateSupersedeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: old.clone(),
            successor_id: new.clone(),
            expect_phase: state.candidates[&old].phase_op_id.clone(),
            idempotency_key: "supersede-old".into(),
        }),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.candidate_landability(&new, Some("lander")).landable);
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&old_reservation],
            &ids::format_rfc3339(Timestamp::now())
        ),
        LeaseDisposition::Orphaned
    );
}

#[test]
fn abandoned_ancestor_blocks_only_when_introduced_after_base_and_ambiguity_fails_closed() {
    let (_temp, store, issue) = setup();
    let old = ids::new_candidate_id();
    let new = ids::new_candidate_id();
    let old_proposal = publish_checked(
        &store,
        &proposal(&old, &issue, COMMIT_A, "propose-abandoned"),
    );
    abandon(&store, &old, "abandon-old");
    publish_checked(
        &store,
        &proposal(&new, &issue, COMMIT_B, "propose-descendant"),
    );
    publish_checked(&store, &approve(&new, "review-descendant"));
    publish_checked(&store, &authorize(&new, "authorize-descendant"));

    let record_relation = |base_relation, relation, key| {
        publish_checked(
            &store,
            &evidence(
                &new,
                ancestry_payload(
                    &new,
                    COMMIT_B,
                    vec![(old.clone(), old_proposal.clone())],
                    vec![GitCandidateRelation {
                        candidate_id: old.clone(),
                        proposal_op_id: old_proposal.clone(),
                        commit_oid: COMMIT_A.into(),
                        base_relation,
                        relation,
                    }],
                ),
                key,
            ),
        );
        reducer::replay_store(&store)
            .unwrap()
            .candidate_landability(&new, Some("lander"))
    };

    // Court A: the abandoned commit adds nothing relative to the immutable base.
    let already_in_base = record_relation(
        Some(GitRelationKind::Ancestor),
        GitRelationKind::Ancestor,
        "already-in-base",
    );
    assert!(already_in_base.landable, "{already_in_base:?}");
    assert!(
        !already_in_base
            .reason_codes
            .contains(&"ancestor_abandoned".into())
    );

    // Court B: the candidate introduces the abandoned commit after its base.
    let introduced = record_relation(
        Some(GitRelationKind::NotAncestor),
        GitRelationKind::Ancestor,
        "introduced-after-base",
    );
    assert!(!introduced.landable);
    assert!(
        introduced
            .reason_codes
            .contains(&"ancestor_abandoned".into())
    );

    // Court C: ambiguous and legacy-missing base proof both fail closed without
    // pretending that introduced-after-base was actually established.
    let ambiguous = record_relation(
        Some(GitRelationKind::Ambiguous),
        GitRelationKind::Ancestor,
        "ambiguous-base",
    );
    assert!(!ambiguous.landable);
    assert!(
        ambiguous
            .reason_codes
            .contains(&"ancestor_ambiguous".into())
    );
    assert!(
        !ambiguous
            .reason_codes
            .contains(&"ancestor_abandoned".into())
    );

    let legacy = record_relation(None, GitRelationKind::Ancestor, "legacy-missing-base");
    assert!(!legacy.landable);
    assert!(legacy.reason_codes.contains(&"ancestor_ambiguous".into()));

    // Court E: an abandoned commit on an unrelated branch remains irrelevant.
    let unrelated = record_relation(
        Some(GitRelationKind::NotAncestor),
        GitRelationKind::NotAncestor,
        "unrelated-branch",
    );
    assert!(unrelated.landable, "{unrelated:?}");
}

#[test]
fn authorization_race_and_idempotency_fail_closed() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal = proposal(&candidate_id, &issue, COMMIT_A, "same-key");
    publish_checked(&store, &proposal);
    let reservation_id = reserve_candidate(&store, &candidate_id, "proposer");
    let mut exact_retry = proposal.clone();
    if let Op::CandidatePropose(op) = &mut exact_retry {
        op.ts = ids::format_rfc3339(Timestamp::now());
    }
    publish_checked(&store, &exact_retry);
    assert_eq!(reducer::replay_store(&store).unwrap().candidates.len(), 1);

    let conflicting = Op::CandidateReview(CandidateReviewOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.clone(),
        verdict: ReviewVerdict::Comment,
        body: None,
        evidence_refs: Vec::new(),
        expect_review: None,
        idempotency_key: "same-key".into(),
    });
    let name = publish::publish_op(&store, &conflicting).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    assert!(
        state
            .rejection_reason(name.as_str())
            .unwrap()
            .contains("idempotency")
    );

    let authorization_op = publish_checked(&store, &authorize(&candidate_id, "grant-race"));
    let revoke = Op::CandidateRevoke(CandidateRevokeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "authorizer".into(),
        candidate_id: candidate_id.clone(),
        expect_authorization: authorization_op,
        reason: Some("stop".into()),
        idempotency_key: "revoke-race".into(),
    });
    let revoke_op = publish_checked(&store, &revoke);
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state
            .candidate_landability(&candidate_id, Some("lander"))
            .reason_codes
            .contains(&"authorization_revoked".into())
    );
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&reservation_id],
            &ids::format_rfc3339(Timestamp::now())
        ),
        LeaseDisposition::Orphaned
    );

    publish_checked(
        &store,
        &Op::CandidateAuthorize(CandidateAuthorizeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.clone(),
            status: AuthorizationStatus::Granted,
            grantees: vec!["lander".into()],
            conditions: Vec::new(),
            expect_authorization: Some(revoke_op),
            idempotency_key: "grant-after-revoke".into(),
        }),
    );
    publish_checked(
        &store,
        &make_reserve_close(
            "proposer".into(),
            reservation_id.clone(),
            None,
            Timestamp::now(),
        ),
    );
    let replacement = reserve_candidate(&store, &candidate_id, "proposer");
    let state = reducer::replay_store(&store).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());
    assert_eq!(
        state.reservation_disposition(&state.reservations[&reservation_id], &now),
        LeaseDisposition::Closed
    );
    assert_eq!(
        state.reservation_disposition(&state.reservations[&replacement], &now),
        LeaseDisposition::Active
    );
}

#[test]
fn review_cas_and_terminal_abandon_are_one_way() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(&candidate_id, &issue, COMMIT_A, "propose-terminal"),
    );
    let reservation_id = reserve_candidate(&store, &candidate_id, "proposer");
    publish_checked(&store, &approve(&candidate_id, "review-first"));
    let stale_review = Op::CandidateReview(CandidateReviewOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "reviewer".into(),
        candidate_id: candidate_id.clone(),
        verdict: ReviewVerdict::Block,
        body: None,
        evidence_refs: Vec::new(),
        expect_review: None,
        idempotency_key: "review-racer".into(),
    });
    let stale_name = publish::publish_op(&store, &stale_review).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(stale_name.as_str()));

    publish_checked(
        &store,
        &Op::CandidateAbandon(CandidateAbandonOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.clone(),
            expect_phase: state.candidates[&candidate_id].phase_op_id.clone(),
            reason: Some("obsolete".into()),
            idempotency_key: "abandon".into(),
        }),
    );
    let late_review = approve(&candidate_id, "review-after-abandon");
    let late_name = publish::publish_op(&store, &late_review).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(late_name.as_str()));
    assert_eq!(state.candidates[&candidate_id].phase.as_str(), "abandoned");
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&reservation_id],
            &ids::format_rfc3339(Timestamp::now())
        ),
        LeaseDisposition::Orphaned
    );
}

#[test]
fn candidate_reservation_rejects_undeclared_paths_and_terminal_candidates() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(&candidate_id, &issue, COMMIT_A, "propose-reserve-policy"),
    );
    let bad_path = publish::publish_op(
        &store,
        &make_reserve_open(
            "proposer".into(),
            ids::new_reservation_id(),
            candidate_id.clone(),
            vec!["src/not-declared.rs".into()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(bad_path.as_str()));
    assert!(
        state
            .rejection_reason(bad_path.as_str())
            .unwrap()
            .contains("not declared")
    );

    publish_checked(
        &store,
        &Op::CandidateAbandon(CandidateAbandonOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.clone(),
            expect_phase: state.candidates[&candidate_id].phase_op_id.clone(),
            reason: None,
            idempotency_key: "abandon-reserve-policy".into(),
        }),
    );
    let terminal = publish::publish_op(
        &store,
        &make_reserve_open(
            "proposer".into(),
            ids::new_reservation_id(),
            candidate_id,
            vec!["src/lib.rs".into()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(terminal.as_str()));
    assert!(
        state
            .rejection_reason(terminal.as_str())
            .unwrap()
            .contains("require a pending candidate")
    );
}

#[test]
fn concurrent_revoke_invalidates_only_reservations_already_active_at_its_replay_position() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(&candidate_id, &issue, COMMIT_A, "propose-revoke-race"),
    );
    let authorization_op = publish_checked(&store, &authorize(&candidate_id, "authorize-race"));
    let same_ts = ids::format_rfc3339(Timestamp::now());
    let reservation_id = ids::new_reservation_id();
    let mut reserve = make_reserve_open(
        "proposer".into(),
        reservation_id.clone(),
        candidate_id.clone(),
        vec!["src/lib.rs".into()],
        3600,
        Timestamp::now(),
    );
    if let Op::ReserveOpen(op) = &mut reserve {
        op.op.clear();
        op.ts = same_ts.clone();
    }
    let revoke = Op::CandidateRevoke(CandidateRevokeOp {
        v: 1,
        op: String::new(),
        ts: same_ts,
        actor: "authorizer".into(),
        candidate_id: candidate_id.clone(),
        expect_authorization: authorization_op,
        reason: Some("race".into()),
        idempotency_key: "revoke-binding-race".into(),
    });
    let reserve_name = publish::publish_op(&store, &reserve).unwrap();
    let revoke_name = publish::publish_op(&store, &revoke).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.was_accepted(reserve_name.as_str()));
    assert!(state.was_accepted(revoke_name.as_str()));
    let disposition = state.reservation_disposition(
        &state.reservations[&reservation_id],
        &ids::format_rfc3339(Timestamp::now()),
    );
    let expected = if revoke_name.as_str() > reserve_name.as_str() {
        LeaseDisposition::Orphaned
    } else {
        LeaseDisposition::Active
    };
    assert_eq!(disposition, expected);
    assert_eq!(
        format!("{:?}", reducer::replay_store(&store).unwrap()),
        format!("{:?}", state)
    );
}

#[test]
fn external_receipts_bind_exact_oid_and_allow_required_producers_to_share_a_digest() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let mut proposed_op = proposal(&candidate_id, &issue, COMMIT_A, "propose-evidence");
    let Op::CandidatePropose(proposed) = &mut proposed_op else {
        unreachable!()
    };
    proposed.evidence_requirements.push(EvidenceRequirement {
        name: "tests".into(),
        kind: "ci".into(),
        producers: vec!["ci-one".into(), "ci-two".into()],
    });
    publish_checked(&store, &proposed_op);

    let payload = CandidateEvidencePayload::External {
        digest: "sha256:same-artifact".into(),
        detail: Some("shared result".into()),
    };
    let make_external = |actor: &str, oid: &str, key: &str| {
        Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: actor.into(),
            candidate_id: candidate_id.clone(),
            candidate_oid: oid.into(),
            evidence_id: mote::candidate::evidence_id(&payload).unwrap(),
            name: "tests".into(),
            evidence_kind: "ci".into(),
            producer_tool: "fixture-ci".into(),
            outcome: EvidenceOutcome::Pass,
            payload: payload.clone(),
            refs: Vec::new(),
            idempotency_key: key.into(),
        })
    };
    let wrong =
        publish::publish_op(&store, &make_external("ci-one", COMMIT_B, "wrong-object")).unwrap();
    assert!(
        !reducer::replay_store(&store)
            .unwrap()
            .was_accepted(wrong.as_str())
    );
    publish_checked(&store, &make_external("ci-one", COMMIT_A, "ci-one-pass"));
    publish_checked(&store, &make_external("ci-two", COMMIT_A, "ci-two-pass"));
    let state = reducer::replay_store(&store).unwrap();
    let evidence = &state.candidates[&candidate_id].evidence;
    assert_eq!(
        evidence
            .values()
            .filter(|receipt| receipt.name == "tests")
            .count(),
        2
    );
}

#[test]
fn legacy_store_replays_with_empty_candidate_map() {
    let (_temp, store, issue) = setup();
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.candidates.is_empty());
    assert!(state.beads.contains_key(&issue));
}

fn run_git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run_mote(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn git_probe_distinguishes_already_in_base_introduced_and_unrelated_relations() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(temp.path(), &["config", "user.email", "test@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Test"]);
    std::fs::write(temp.path().join("work.txt"), "root\n").unwrap();
    run_git(temp.path(), &["add", "work.txt"]);
    run_git(temp.path(), &["commit", "-qm", "root"]);
    let root = run_git(temp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(temp.path().join("work.txt"), "known\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "known candidate"]);
    let known_oid = run_git(temp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(temp.path().join("work.txt"), "descendant\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "descendant"]);
    let descendant = run_git(temp.path(), &["rev-parse", "HEAD"]);

    let identity =
        mote::candidate::probe_ancestry(temp.path(), &descendant, &known_oid, &[]).unwrap();
    let known = [KnownCandidate {
        candidate_id: "cand-known".into(),
        proposal_op_id: "op-known".into(),
        repository_id: identity.repository_id,
        commit_oid: known_oid.clone(),
    }];

    let already =
        mote::candidate::probe_ancestry(temp.path(), &descendant, &known_oid, &known).unwrap();
    assert_eq!(
        already.candidate_relations[0].base_relation,
        Some(GitRelationKind::Ancestor)
    );
    assert_eq!(
        already.candidate_relations[0].relation,
        GitRelationKind::Ancestor
    );

    let introduced =
        mote::candidate::probe_ancestry(temp.path(), &descendant, &root, &known).unwrap();
    assert_eq!(
        introduced.candidate_relations[0].base_relation,
        Some(GitRelationKind::NotAncestor)
    );
    assert_eq!(
        introduced.candidate_relations[0].relation,
        GitRelationKind::Ancestor
    );

    run_git(temp.path(), &["checkout", "-qb", "unrelated", &root]);
    std::fs::write(temp.path().join("work.txt"), "unrelated\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "unrelated"]);
    let unrelated_oid = run_git(temp.path(), &["rev-parse", "HEAD"]);
    let unrelated =
        mote::candidate::probe_ancestry(temp.path(), &unrelated_oid, &root, &known).unwrap();
    assert_eq!(
        unrelated.candidate_relations[0].base_relation,
        Some(GitRelationKind::NotAncestor)
    );
    assert_eq!(
        unrelated.candidate_relations[0].relation,
        GitRelationKind::NotAncestor
    );
}

#[test]
fn candidate_cli_happy_path_and_json_schema() {
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
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("CLI candidate".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();

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
            "--reviewer",
            "reviewer",
            "--idempotency-key",
            "cli-propose",
        ],
    );
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let proposal_op_id = proposed["proposal_op_id"].as_str().unwrap();
    assert_eq!(proposed["phase"]["value"], "pending");
    assert!(proposed["identity"]["commit_oid"].as_str().unwrap().len() == 40);
    assert!(proposed["landability"]["reason_codes"].is_array());
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();

    let reserved = run_mote(
        temp.path(),
        &[
            "--actor",
            "proposer",
            "reserve",
            "work.txt",
            "--candidate",
            candidate_id,
        ],
    );
    assert!(
        reserved.status.success(),
        "{}",
        String::from_utf8_lossy(&reserved.stderr)
    );
    let reservation_id = String::from_utf8(reserved.stdout)
        .unwrap()
        .trim()
        .to_string();
    let candidate_with_reservation =
        run_mote(temp.path(), &["--json", "candidate", "show", candidate_id]);
    let candidate_with_reservation: serde_json::Value =
        serde_json::from_slice(&candidate_with_reservation.stdout).unwrap();
    assert_eq!(
        candidate_with_reservation["reservations"][0]["reservation_id"],
        reservation_id
    );
    assert_eq!(
        candidate_with_reservation["reservations"][0]["disposition"],
        "active"
    );
    let preflight = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "reviewer",
            "preflight",
            "--candidate",
            candidate_id,
            "--paths",
            "work.txt",
        ],
    );
    assert_eq!(preflight.status.code(), Some(2));
    let preflight: serde_json::Value = serde_json::from_slice(&preflight.stdout).unwrap();
    assert_eq!(preflight["binding_kind"], "candidate");
    assert_eq!(preflight["candidate"], candidate_id);
    assert_eq!(preflight["conflicts"][0]["disposition"], "active");

    let unauthorized = run_mote(
        temp.path(),
        &[
            "--actor",
            "intruder",
            "candidate",
            "review",
            candidate_id,
            "approve",
            "--idempotency-key",
            "bad-review",
        ],
    );
    assert_eq!(unauthorized.status.code(), Some(2));

    let reviewed = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "reviewer",
            "candidate",
            "review",
            candidate_id,
            "approve",
            "--idempotency-key",
            "cli-review",
        ],
    );
    assert!(
        reviewed.status.success(),
        "{}",
        String::from_utf8_lossy(&reviewed.stderr)
    );
    let authorized = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "authorizer",
            "candidate",
            "authorize",
            candidate_id,
            "--grantee",
            "lander",
            "--idempotency-key",
            "cli-authorize",
        ],
    );
    assert!(
        authorized.status.success(),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    let authorized: serde_json::Value = serde_json::from_slice(&authorized.stdout).unwrap();
    let authorization_op = authorized["authorization"]["op_id"].as_str().unwrap();
    assert_eq!(authorized["landability"]["landable"], true);

    let landed = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "lander",
            "candidate",
            "landed",
            candidate_id,
            "--target",
            "HEAD",
            "--expect-phase",
            phase_op,
            "--expect-authorization",
            authorization_op,
            "--idempotency-key",
            "cli-landed",
        ],
    );
    assert!(
        landed.status.success(),
        "{}",
        String::from_utf8_lossy(&landed.stderr)
    );
    let landed: serde_json::Value = serde_json::from_slice(&landed.stdout).unwrap();
    assert_eq!(landed["phase"]["value"], "landed");
    assert_eq!(landed["authorization"]["status"], "consumed");
    assert_eq!(landed["reservations"][0]["disposition"], "orphaned");

    let listed = run_mote(
        temp.path(),
        &["--json", "candidate", "list", "--phase", "landed"],
    );
    assert!(listed.status.success());
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let in_flight = run_mote(temp.path(), &["--json", "in-flight", "--no-git"]);
    assert!(in_flight.status.success());
    let in_flight: serde_json::Value = serde_json::from_slice(&in_flight.stdout).unwrap();
    assert_eq!(in_flight["candidates"][0]["candidate_id"], candidate_id);
    assert_eq!(in_flight["candidates"][0]["phase"]["value"], "landed");
    assert!(
        in_flight["candidates"][0]["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == "phase_not_pending")
    );

    let events = run_mote(temp.path(), &["--json", "events", "--kind", "candidate"]);
    assert!(events.status.success());
    let candidate_events: Vec<serde_json::Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(candidate_events.len() >= 5);
    assert!(
        candidate_events
            .iter()
            .all(|event| event["category"] == "candidate")
    );
    assert!(
        candidate_events
            .iter()
            .any(|event| event["type"] == "candidate.landed")
    );
    let landed_event = candidate_events
        .iter()
        .find(|event| event["type"] == "candidate.landed")
        .unwrap();
    assert_eq!(
        landed_event["data"]["lease_effects"]["orphaned_reservations"][0]["reservation_id"],
        reservation_id
    );

    let reservation_events = run_mote(temp.path(), &["--json", "events", "--kind", "reservation"]);
    let reservation_events: Vec<serde_json::Value> = String::from_utf8(reservation_events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let opened_event = reservation_events
        .iter()
        .find(|event| event["type"] == "reservation.opened")
        .unwrap();
    assert_eq!(opened_event["data"]["binding_kind"], "candidate");
    assert_eq!(opened_event["data"]["candidate_id"], candidate_id);

    let after_proposal = run_mote(
        temp.path(),
        &[
            "--json",
            "events",
            "--kind",
            "candidate",
            "--after",
            proposal_op_id,
        ],
    );
    assert!(after_proposal.status.success());
    let after_events: Vec<serde_json::Value> = String::from_utf8(after_proposal.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!after_events.is_empty());
    assert!(
        after_events
            .iter()
            .all(|event| event["type"] != "candidate.proposed")
    );
}

#[test]
fn evidence_refresh_clears_abandoned_commit_already_in_base_without_rewriting_history() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(temp.path(), &["config", "user.email", "test@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Test"]);
    std::fs::write(temp.path().join("work.txt"), "root\n").unwrap();
    run_git(temp.path(), &["add", "work.txt"]);
    run_git(temp.path(), &["commit", "-qm", "root"]);
    let root = run_git(temp.path(), &["rev-parse", "HEAD"]);
    std::fs::write(temp.path().join("work.txt"), "landed\n").unwrap();
    run_git(
        temp.path(),
        &["commit", "-qam", "landed outside candidate flow"],
    );
    let landed = run_git(temp.path(), &["rev-parse", "HEAD"]);

    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("Base-relative refresh".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();

    let old = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "proposer",
            "candidate",
            "propose",
            "--issue",
            &issue,
            "--commit",
            &landed,
            "--base",
            &root,
            "--path",
            "work.txt",
            "--authorizer",
            "authorizer",
            "--reviewer",
            "reviewer",
            "--idempotency-key",
            "old-candidate",
        ],
    );
    assert!(
        old.status.success(),
        "{}",
        String::from_utf8_lossy(&old.stderr)
    );
    let old: serde_json::Value = serde_json::from_slice(&old.stdout).unwrap();
    let old_id = old["candidate_id"].as_str().unwrap().to_string();
    abandon(&store, &old_id, "honest-abandon-after-landing");

    std::fs::write(temp.path().join("work.txt"), "descendant\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "later candidate"]);
    let new = run_mote(
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
            &landed,
            "--path",
            "work.txt",
            "--authorizer",
            "authorizer",
            "--reviewer",
            "reviewer",
            "--idempotency-key",
            "new-candidate",
        ],
    );
    assert!(
        new.status.success(),
        "{}",
        String::from_utf8_lossy(&new.stderr)
    );
    let new: serde_json::Value = serde_json::from_slice(&new.stdout).unwrap();
    let new_id = new["candidate_id"].as_str().unwrap().to_string();
    publish_checked(&store, &approve(&new_id, "review-refreshed"));
    publish_checked(&store, &authorize(&new_id, "authorize-refreshed"));
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_landability(&new_id, Some("lander"))
            .landable
    );

    // Simulate the accepted legacy receipt that knew the old commit reached
    // the tip but did not record its relation to the immutable base.
    let state = reducer::replay_store(&store).unwrap();
    let current = state.candidates[&new_id]
        .evidence
        .values()
        .find(|record| record.name == GIT_ANCESTRY_EVIDENCE)
        .unwrap();
    let mut legacy_payload = current.payload.clone();
    let CandidateEvidencePayload::GitAncestry(legacy) = &mut legacy_payload else {
        panic!("expected ancestry receipt")
    };
    let old_relation = legacy
        .candidate_relations
        .iter_mut()
        .find(|relation| relation.candidate_id == old_id)
        .unwrap();
    assert_eq!(old_relation.base_relation, Some(GitRelationKind::Ancestor));
    assert_eq!(old_relation.relation, GitRelationKind::Ancestor);
    old_relation.base_relation = None;
    publish_checked(
        &store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: new_id.clone(),
            candidate_oid: state.candidates[&new_id].commit_oid.clone(),
            evidence_id: mote::candidate::evidence_id(&legacy_payload).unwrap(),
            name: GIT_ANCESTRY_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "legacy git probe".into(),
            outcome: EvidenceOutcome::Pass,
            payload: legacy_payload,
            refs: Vec::new(),
            idempotency_key: "legacy-receipt".into(),
        }),
    );
    let blocked = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&new_id, Some("lander"));
    assert!(!blocked.landable);
    assert!(blocked.reason_codes.contains(&"ancestor_ambiguous".into()));

    let before_refresh = store.list_op_filenames().unwrap().len();
    let refreshed = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "proposer",
            "candidate",
            "evidence",
            "refresh",
            &new_id,
            "--idempotency-key",
            "refresh-base-relative-proof",
        ],
    );
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    assert_eq!(store.list_op_filenames().unwrap().len(), before_refresh + 1);
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.candidates[&old_id].phase.as_str(), "abandoned");
    assert!(
        state
            .candidate_landability(&new_id, Some("lander"))
            .landable
    );
    let refreshed = state.candidates[&new_id]
        .evidence
        .values()
        .find(|record| record.name == GIT_ANCESTRY_EVIDENCE)
        .unwrap();
    let CandidateEvidencePayload::GitAncestry(refreshed) = &refreshed.payload else {
        panic!("expected ancestry receipt")
    };
    let relation = refreshed
        .candidate_relations
        .iter()
        .find(|relation| relation.candidate_id == old_id)
        .unwrap();
    assert_eq!(relation.base_relation, Some(GitRelationKind::Ancestor));
    assert_eq!(relation.relation, GitRelationKind::Ancestor);
}
