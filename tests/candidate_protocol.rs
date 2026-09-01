use jiff::Timestamp;
use std::process::Command;
use tempfile::TempDir;

use mote::candidate::{
    AuthorizationStatus, CandidateEvidenceOperatorOverride, CandidateEvidencePayload,
    CandidateOperatorOverride, CandidatePolicySnapshot, CandidateReconciliationAuthority,
    CandidateReconciliationRepositoryBridge, CandidateSnapshotProvenance,
    CandidateSupersedeRecovery, CandidateSupersessionAuthority, EvidenceOutcome,
    EvidenceRequirement, GIT_ANCESTRY_EVIDENCE, GIT_LANDING_EVIDENCE, GIT_REACHABILITY_EVIDENCE,
    GIT_RECONCILIATION_REACHABILITY_EVIDENCE, GIT_RELATION_SCHEMA_V2, GitAncestryReceipt,
    GitCandidateRelation, GitLandingReceipt, GitObjectAvailabilityReceipt, GitReachabilityReceipt,
    GitRelationKind, KnownCandidate, ReviewVerdict, evidence_id,
};
use mote::ids;
use mote::op::{
    CandidateAbandonOp, CandidateAuthorizeOp, CandidateEvidenceOp, CandidateLandedOp,
    CandidateProposeOp, CandidateReconcileOp, CandidateReviewOp, CandidateRevokeOp,
    CandidateSupersedeOp, Op, ScalarSet, make_create, make_reserve_close, make_reserve_open,
};
use mote::state::LeaseDisposition;
use mote::{publish, reducer, repo::Store};

const BASE: &str = "1111111111111111111111111111111111111111";
const COMMIT_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const COMMIT_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const COMMIT_C: &str = "cccccccccccccccccccccccccccccccccccccccc";
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

fn publish_rejected(store: &Store, op: &Op, reason_fragment: &str) -> String {
    let name = publish::publish_op(store, op).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    let reason = state.rejection_reason(name.as_str()).unwrap();
    assert!(
        reason.contains(reason_fragment),
        "expected `{reason_fragment}` in `{reason}`"
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
        landing_repository_id: None,
        object_source: None,
        object_format: "sha1".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        paths: vec!["src/lib.rs".into()],
        authorizer: "authorizer".into(),
        reviewers: vec!["reviewer".into()],
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

fn proposal_with_authorizer(
    candidate_id: &str,
    issue: &str,
    commit: &str,
    authorizer: &str,
    key: &str,
) -> Op {
    let mut op = proposal(candidate_id, issue, commit, key);
    let Op::CandidatePropose(proposal) = &mut op else {
        unreachable!()
    };
    proposal.authorizer = authorizer.into();
    op
}

fn ancestry_payload(
    candidate_id: &str,
    commit: &str,
    covered: Vec<(String, String)>,
    relations: Vec<GitCandidateRelation>,
) -> CandidateEvidencePayload {
    let _ = candidate_id;
    CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        relation_schema: 1,
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        common_dir_hash: "common".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        base_is_ancestor: Some(true),
        candidate_relations: relations,
        covered_candidates: covered,
        producer_snapshot: None,
        git_version: "git version test".into(),
        detail: None,
    })
}

fn v2_ancestry_payload(
    commit: &str,
    observed: Vec<(String, String)>,
    relations: Vec<GitCandidateRelation>,
    snapshot_digest: &str,
) -> CandidateEvidencePayload {
    CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        relation_schema: GIT_RELATION_SCHEMA_V2,
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        common_dir_hash: "common".into(),
        commit_oid: commit.into(),
        base_oid: BASE.into(),
        parent_oids: vec![BASE.into()],
        base_is_ancestor: Some(true),
        candidate_relations: relations,
        covered_candidates: observed.clone(),
        producer_snapshot: Some(Box::new(CandidateSnapshotProvenance {
            store_id: "st-test".into(),
            replayed_op_count: observed.len() as u64,
            observed_candidates: observed,
            replayed_op_ids_digest: snapshot_digest.into(),
            store_git_head: None,
            uncommitted_op_count: None,
        })),
        git_version: "git version test".into(),
        detail: None,
    })
}

fn v2_relation(
    candidate_id: &str,
    proposal_op_id: &str,
    commit_oid: &str,
    base_relation: GitRelationKind,
    tip_relation: GitRelationKind,
    reciprocal_base_relation: GitRelationKind,
    reciprocal_tip_relation: GitRelationKind,
) -> GitCandidateRelation {
    GitCandidateRelation {
        candidate_id: candidate_id.into(),
        proposal_op_id: proposal_op_id.into(),
        commit_oid: commit_oid.into(),
        base_oid: Some(BASE.into()),
        base_relation: Some(base_relation),
        relation: tip_relation,
        subject_to_known_base: Some(reciprocal_base_relation),
        subject_to_known_tip: Some(reciprocal_tip_relation),
    }
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

fn operator_ancestry_evidence(
    candidate_id: &str,
    receipt: GitAncestryReceipt,
    override_basis: CandidateEvidenceOperatorOverride,
    actor: &str,
    refs: Vec<String>,
    key: &str,
) -> Op {
    let candidate_oid = receipt.commit_oid.clone();
    let producer_tool = receipt.git_version.clone();
    let payload = CandidateEvidencePayload::GitAncestryOverride {
        receipt,
        override_basis,
    };
    Op::CandidateEvidence(CandidateEvidenceOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: actor.into(),
        candidate_id: candidate_id.into(),
        candidate_oid,
        evidence_id: evidence_id(&payload).unwrap(),
        name: GIT_ANCESTRY_EVIDENCE.into(),
        evidence_kind: "git".into(),
        producer_tool,
        outcome: EvidenceOutcome::Pass,
        payload,
        refs,
        idempotency_key: key.into(),
    })
}

fn reachability_evidence(
    candidate_id: &str,
    actor: &str,
    outcome: EvidenceOutcome,
    reachable: Option<bool>,
    repository_id: &str,
    key: &str,
) -> (Op, String) {
    reachability_evidence_for_commit(
        candidate_id,
        actor,
        outcome,
        reachable,
        repository_id,
        COMMIT_A,
        key,
    )
}

#[allow(clippy::too_many_arguments)]
fn reachability_evidence_for_commit(
    candidate_id: &str,
    actor: &str,
    outcome: EvidenceOutcome,
    reachable: Option<bool>,
    repository_id: &str,
    candidate_oid: &str,
    key: &str,
) -> (Op, String) {
    let payload = CandidateEvidencePayload::GitReachability(GitReachabilityReceipt {
        repository_id: repository_id.into(),
        object_format: "sha1".into(),
        candidate_oid: candidate_oid.into(),
        target_ref: "refs/heads/main".into(),
        observed_target_oid: candidate_oid.into(),
        candidate_reachable: reachable,
        git_version: "git version test".into(),
        detail: None,
    });
    let evidence_id = mote::candidate::evidence_id(&payload).unwrap();
    (
        Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: actor.into(),
            candidate_id: candidate_id.into(),
            candidate_oid: candidate_oid.into(),
            evidence_id: evidence_id.clone(),
            name: GIT_REACHABILITY_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome,
            payload,
            refs: Vec::new(),
            idempotency_key: key.into(),
        }),
        evidence_id,
    )
}

fn reconcile(
    candidate_id: &str,
    actor: &str,
    evidence_id: &str,
    expect_phase: &str,
    policy_snapshot: CandidatePolicySnapshot,
    key: &str,
) -> Op {
    Op::CandidateReconcile(CandidateReconcileOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: actor.into(),
        candidate_id: candidate_id.into(),
        evidence_id: evidence_id.into(),
        target_ref: "refs/heads/main".into(),
        expect_phase: expect_phase.into(),
        authority: CandidateReconciliationAuthority::ProposalAuthorizer,
        override_basis: None,
        repository_bridge: None,
        policy_snapshot,
        idempotency_key: key.into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn override_reconcile(
    candidate_id: &str,
    actor: &str,
    evidence_id: &str,
    expect_phase: &str,
    expect_authorizer: &str,
    authority_refs: Vec<&str>,
    policy_snapshot: CandidatePolicySnapshot,
    key: &str,
) -> Op {
    Op::CandidateReconcile(CandidateReconcileOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: actor.into(),
        candidate_id: candidate_id.into(),
        evidence_id: evidence_id.into(),
        target_ref: "refs/heads/main".into(),
        expect_phase: expect_phase.into(),
        authority: CandidateReconciliationAuthority::ExplicitOperatorOverride,
        override_basis: Some(CandidateOperatorOverride {
            expect_authorizer: expect_authorizer.into(),
            expect_landing_repository_id: REPO.into(),
            reason: "proposal authorizer is unavailable; preserve observed Git state".into(),
            authority_refs: authority_refs.into_iter().map(str::to_string).collect(),
        }),
        repository_bridge: None,
        policy_snapshot,
        idempotency_key: key.into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn containment_recovery(
    predecessor_id: &str,
    successor_id: &str,
    actor: &str,
    expect_phase: &str,
    expect_successor_phase: &str,
    evidence_op_ids: Vec<String>,
    authority: CandidateSupersessionAuthority,
    key: &str,
) -> Op {
    Op::CandidateSupersede(CandidateSupersedeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: actor.into(),
        candidate_id: predecessor_id.into(),
        successor_id: successor_id.into(),
        expect_phase: expect_phase.into(),
        recovery: Some(CandidateSupersedeRecovery {
            authority,
            expect_successor_phase: expect_successor_phase.into(),
            containment_evidence_op_ids: evidence_op_ids,
        }),
        idempotency_key: key.into(),
    })
}

#[test]
fn legacy_git_ancestry_payload_defaults_to_one_directional_schema() {
    let legacy_json = serde_json::json!({
        "kind": "git_ancestry",
        "repository_id": REPO,
        "object_format": "sha1",
        "common_dir_hash": "common",
        "commit_oid": COMMIT_A,
        "base_oid": BASE,
        "parent_oids": [BASE],
        "base_is_ancestor": true,
        "candidate_relations": [{
            "candidate_id": "cand-legacy",
            "proposal_op_id": "op-legacy",
            "commit_oid": COMMIT_B,
            "base_relation": "not_ancestor",
            "relation": "not_ancestor"
        }],
        "covered_candidates": [["cand-legacy", "op-legacy"]],
        "git_version": "git version legacy"
    });
    let payload: CandidateEvidencePayload = serde_json::from_value(legacy_json.clone()).unwrap();

    assert_eq!(
        evidence_id(&payload).unwrap(),
        evidence_id(&legacy_json).unwrap(),
        "materializing the legacy schema default must not change evidence identity"
    );
    let serialized = serde_json::to_value(&payload).unwrap();
    assert!(
        serialized.get("relation_schema").is_none(),
        "schema 1 must retain its historical omitted-field representation"
    );

    let CandidateEvidencePayload::GitAncestry(receipt) = &payload else {
        unreachable!();
    };
    assert_eq!(receipt.relation_schema, 1);
    assert!(receipt.producer_snapshot.is_none());
    assert!(receipt.candidate_relations[0].base_oid.is_none());
    assert!(
        receipt.candidate_relations[0]
            .subject_to_known_base
            .is_none()
    );
    assert!(
        receipt.candidate_relations[0]
            .subject_to_known_tip
            .is_none()
    );

    let current = v2_ancestry_payload(COMMIT_A, Vec::new(), Vec::new(), "current-snapshot");
    assert_eq!(
        serde_json::to_value(current).unwrap()["relation_schema"],
        GIT_RELATION_SCHEMA_V2,
        "schema 2 remains explicit and identity-bearing"
    );
}

#[test]
fn legacy_candidate_supersede_payload_defaults_to_owner_mode() {
    let op: Op = serde_json::from_value(serde_json::json!({
        "v": 1,
        "op": "op-legacy",
        "ts": "2026-08-30T00:00:00Z",
        "actor": "proposer",
        "kind": "candidate_supersede",
        "candidate_id": "cand-01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "successor_id": "cand-01ARZ3NDEKTSV4RRFFQ69G5FAW",
        "expect_phase": "op-proposal",
        "idempotency_key": "legacy-supersede"
    }))
    .unwrap();
    let Op::CandidateSupersede(supersede) = op else {
        unreachable!()
    };
    assert!(supersede.recovery.is_none());
}

#[test]
fn legacy_candidate_reconcile_payload_defaults_to_no_override_basis() {
    let legacy = serde_json::json!({
        "v": 1,
        "op": "op-legacy-reconcile",
        "ts": "2026-08-30T00:00:00Z",
        "actor": "authorizer",
        "kind": "candidate_reconcile",
        "candidate_id": "cand-01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "evidence_id": "evid-legacy",
        "target_ref": "refs/heads/main",
        "expect_phase": "op-proposal",
        "authority": "proposal_authorizer",
        "policy_snapshot": {
            "phase_op_id": "op-proposal",
            "review_op_ids": [],
            "evidence_op_ids": [],
            "pair_evidence_op_ids": [],
            "pre_transition_landability": {
                "landable": false,
                "reason_codes": [],
                "reasons": []
            }
        },
        "idempotency_key": "legacy-reconcile"
    });
    let op: Op = serde_json::from_value(legacy).unwrap();
    let Op::CandidateReconcile(reconcile) = &op else {
        unreachable!()
    };
    assert_eq!(
        reconcile.authority,
        CandidateReconciliationAuthority::ProposalAuthorizer
    );
    assert!(reconcile.override_basis.is_none());
    assert!(reconcile.repository_bridge.is_none());
    assert!(
        serde_json::to_value(&op)
            .unwrap()
            .get("override_basis")
            .is_none(),
        "legacy proposal-authorizer reconciliation retains its historical wire shape"
    );
    assert!(
        serde_json::to_value(&op)
            .unwrap()
            .get("repository_bridge")
            .is_none(),
        "legacy reconciliation does not acquire a synthetic repository bridge"
    );
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
        role: None,
        idempotency_key: key.into(),
    })
}

fn authorize(candidate_id: &str, key: &str) -> Op {
    authorize_as(candidate_id, "authorizer", key)
}

fn authorize_as(candidate_id: &str, actor: &str, key: &str) -> Op {
    Op::CandidateAuthorize(CandidateAuthorizeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: actor.into(),
        candidate_id: candidate_id.into(),
        status: AuthorizationStatus::Granted,
        grantees: vec!["lander".into()],
        conditions: Vec::new(),
        expect_authorization: None,
        idempotency_key: key.into(),
    })
}

fn abandon(store: &Store, candidate_id: &str, key: &str) -> String {
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
    )
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
        target_ref_full_name: None,
        before_tip: Some(BASE.into()),
        after_tip: COMMIT_A.into(),
        landing_effect_paths: Vec::new(),
        candidate_reachable: Some(true),
        authorization_op_id: authorization_op.clone(),
        basis_op_ids: Vec::new(),
        target_scope_evidence_id: None,
        target_scope_op_id: None,
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
fn out_of_band_reconciliation_preserves_policy_and_resolves_descendants() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &proposal(&candidate_id, &issue, COMMIT_A, "reconcile-proposal"),
    );
    let reservation_id = reserve_candidate(&store, &candidate_id, "proposer");
    publish_checked(
        &store,
        &evidence(
            &candidate_id,
            ancestry_payload(&candidate_id, COMMIT_A, Vec::new(), Vec::new()),
            "reconcile-ancestry",
        ),
    );

    let (independent_reachability_evidence, independent_evidence_id) = reachability_evidence(
        &candidate_id,
        "intruder",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "reconcile-wrong-producer",
    );
    publish_checked(&store, &independent_reachability_evidence);
    let state = reducer::replay_store(&store).unwrap();
    let independent_snapshot = state.candidate_policy_snapshot(&candidate_id).unwrap();
    publish_rejected(
        &store,
        &reconcile(
            &candidate_id,
            "intruder",
            &independent_evidence_id,
            &proposal_op,
            independent_snapshot,
            "reconcile-independent-fact-no-authority",
        ),
        "proposal-authorizer authority",
    );

    let (failed_evidence, failed_id) = reachability_evidence(
        &candidate_id,
        "authorizer",
        EvidenceOutcome::Fail,
        Some(false),
        REPO,
        "reconcile-not-ancestor",
    );
    publish_checked(&store, &failed_evidence);
    let state = reducer::replay_store(&store).unwrap();
    let failed_snapshot = state.candidate_policy_snapshot(&candidate_id).unwrap();
    publish_rejected(
        &store,
        &reconcile(
            &candidate_id,
            "authorizer",
            &failed_id,
            &proposal_op,
            failed_snapshot,
            "reconcile-failed-receipt",
        ),
        "passing exact-reachability receipt",
    );

    let (passing_evidence, evidence_id) = reachability_evidence(
        &candidate_id,
        "authorizer",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "reconcile-passing-receipt",
    );
    publish_checked(&store, &passing_evidence);
    let state = reducer::replay_store(&store).unwrap();
    let stale_snapshot = state.candidate_policy_snapshot(&candidate_id).unwrap();
    assert!(
        stale_snapshot
            .pre_transition_landability
            .reason_codes
            .contains(&"review_missing".into())
    );
    assert!(
        stale_snapshot
            .pre_transition_landability
            .reason_codes
            .contains(&"authorization_absent".into())
    );
    publish_rejected(
        &store,
        &reconcile(
            &candidate_id,
            "intruder",
            &evidence_id,
            &proposal_op,
            stale_snapshot.clone(),
            "reconcile-wrong-authority",
        ),
        "proposal-authorizer authority",
    );

    publish_checked(&store, &approve(&candidate_id, "reconcile-review-race"));
    publish_rejected(
        &store,
        &reconcile(
            &candidate_id,
            "authorizer",
            &evidence_id,
            &proposal_op,
            stale_snapshot,
            "reconcile-stale-policy",
        ),
        "stale reconciliation policy snapshot CAS",
    );

    let authorization_op =
        publish_checked(&store, &authorize(&candidate_id, "reconcile-authorization"));
    let landing_payload = CandidateEvidencePayload::GitLanding(GitLandingReceipt {
        repository_id: REPO.into(),
        object_format: "sha1".into(),
        candidate_oid: COMMIT_A.into(),
        target_ref: "refs/heads/main".into(),
        target_ref_full_name: None,
        before_tip: Some(BASE.into()),
        after_tip: COMMIT_A.into(),
        landing_effect_paths: Vec::new(),
        candidate_reachable: Some(true),
        authorization_op_id: authorization_op.clone(),
        basis_op_ids: Vec::new(),
        target_scope_evidence_id: None,
        target_scope_op_id: None,
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
            idempotency_key: "reconcile-governed-landing-receipt".into(),
        }),
    );

    let state = reducer::replay_store(&store).unwrap();
    let current_snapshot = state.candidate_policy_snapshot(&candidate_id).unwrap();
    assert!(current_snapshot.pre_transition_landability.landable);
    let reconciliation_op = publish_checked(
        &store,
        &reconcile(
            &candidate_id,
            "authorizer",
            &evidence_id,
            &proposal_op,
            current_snapshot.clone(),
            "reconcile-success",
        ),
    );
    let first = format!("{:?}", reducer::replay_store(&store).unwrap());
    let second = format!("{:?}", reducer::replay_store(&store).unwrap());
    assert_eq!(first, second);

    let state = reducer::replay_store(&store).unwrap();
    let candidate = &state.candidates[&candidate_id];
    assert_eq!(candidate.phase.as_str(), "landed_out_of_band");
    assert_eq!(candidate.phase_op_id, reconciliation_op);
    assert!(candidate.landed.is_none());
    let authorization = candidate.authorization.as_ref().unwrap();
    assert_eq!(authorization.op_id, authorization_op);
    assert_eq!(authorization.status, AuthorizationStatus::Granted);
    let recorded = candidate.reconciled.as_ref().unwrap();
    assert_eq!(recorded.policy_snapshot, current_snapshot);
    assert_eq!(
        recorded.policy_snapshot.authorization_op_id.as_deref(),
        Some(authorization_op.as_str())
    );
    assert_eq!(
        recorded.policy_snapshot.authorization_status,
        Some(AuthorizationStatus::Granted)
    );
    assert_eq!(recorded.target_oid, COMMIT_A);
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&reservation_id],
            &ids::format_rfc3339(Timestamp::now())
        ),
        LeaseDisposition::Orphaned
    );

    publish_rejected(
        &store,
        &Op::CandidateRevoke(CandidateRevokeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.clone(),
            expect_authorization: authorization_op.clone(),
            reason: Some("lost terminal race".into()),
            idempotency_key: "reconcile-late-revoke".into(),
        }),
        "pending phase",
    );
    publish_rejected(
        &store,
        &Op::CandidateLanded(CandidateLandedOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "lander".into(),
            candidate_id: candidate_id.clone(),
            evidence_id: landing_evidence_id,
            expect_phase: proposal_op.clone(),
            expect_authorization: authorization_op,
            target_ref: "refs/heads/main".into(),
            idempotency_key: "reconcile-late-governed-landing".into(),
        }),
        "current pending phase",
    );

    let descendant_id = ids::new_candidate_id();
    let descendant_proposal = publish_checked(
        &store,
        &proposal(
            &descendant_id,
            &issue,
            COMMIT_B,
            "reconcile-descendant-proposal",
        ),
    );
    let observed = vec![
        (candidate_id.clone(), proposal_op.clone()),
        (descendant_id.clone(), descendant_proposal.clone()),
    ];
    publish_checked(
        &store,
        &evidence(
            &descendant_id,
            v2_ancestry_payload(
                COMMIT_B,
                observed,
                vec![v2_relation(
                    &candidate_id,
                    &proposal_op,
                    COMMIT_A,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::Ancestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                )],
                "reconcile-descendant-snapshot",
            ),
            "reconcile-descendant-ancestry",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    let descendant_landability = state.candidate_landability(&descendant_id, None);
    assert!(
        !descendant_landability
            .reason_codes
            .contains(&"ancestor_pending".into())
    );
    assert!(
        !descendant_landability
            .reason_codes
            .contains(&"ancestor_supersession_unresolved".into())
    );
}

#[test]
fn explicit_operator_override_is_audited_and_does_not_impersonate_the_authorizer() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &proposal(
            &candidate_id,
            &issue,
            COMMIT_A,
            "operator-override-proposal",
        ),
    );
    let (reachability, evidence_id) = reachability_evidence(
        &candidate_id,
        "recovery-operator",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "operator-override-reachability",
    );
    publish_checked(&store, &reachability);
    let initial_snapshot = reducer::replay_store(&store)
        .unwrap()
        .candidate_policy_snapshot(&candidate_id)
        .unwrap();

    let mut missing_basis = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &evidence_id,
        &proposal_op,
        "authorizer",
        vec!["appointment-record"],
        initial_snapshot.clone(),
        "operator-override-missing-basis",
    );
    let Op::CandidateReconcile(operation) = &mut missing_basis else {
        unreachable!()
    };
    operation.override_basis = None;
    publish_rejected(&store, &missing_basis, "exact original-authorizer CAS");

    let wrong_authorizer = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &evidence_id,
        &proposal_op,
        "caller-selected-authorizer",
        vec!["appointment-record"],
        initial_snapshot.clone(),
        "operator-override-wrong-authorizer",
    );
    publish_rejected(&store, &wrong_authorizer, "exact original-authorizer CAS");

    let mut wrong_repository = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &evidence_id,
        &proposal_op,
        "authorizer",
        vec!["appointment-record"],
        initial_snapshot.clone(),
        "operator-override-wrong-repository",
    );
    let Op::CandidateReconcile(operation) = &mut wrong_repository else {
        unreachable!()
    };
    operation
        .override_basis
        .as_mut()
        .unwrap()
        .expect_landing_repository_id = "repo-caller-selected".into();
    publish_rejected(&store, &wrong_repository, "landing-repository CAS");

    let unsorted_refs = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &evidence_id,
        &proposal_op,
        "authorizer",
        vec!["post-z", "post-a"],
        initial_snapshot.clone(),
        "operator-override-unsorted-refs",
    );
    publish_rejected(&store, &unsorted_refs, "sorted non-empty authority refs");

    let authorizer_override = override_reconcile(
        &candidate_id,
        "authorizer",
        &evidence_id,
        &proposal_op,
        "authorizer",
        vec!["appointment-record"],
        initial_snapshot.clone(),
        "operator-override-authorizer-misuse",
    );
    publish_rejected(&store, &authorizer_override, "distinct explicit operator");

    publish_checked(&store, &approve(&candidate_id, "operator-override-review"));
    let authorization_op = publish_checked(
        &store,
        &authorize(&candidate_id, "operator-override-authorization"),
    );
    let stale = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &evidence_id,
        &proposal_op,
        "authorizer",
        vec!["appointment-record"],
        initial_snapshot,
        "operator-override-stale-policy",
    );
    publish_rejected(&store, &stale, "stale reconciliation policy snapshot CAS");

    let current = reducer::replay_store(&store).unwrap();
    let current_snapshot = current.candidate_policy_snapshot(&candidate_id).unwrap();
    let operation = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &evidence_id,
        &proposal_op,
        "authorizer",
        vec!["appointment-ack", "appointment-record"],
        current_snapshot.clone(),
        "operator-override-success",
    );
    let reconciliation_op = publish_checked(&store, &operation);

    let state = reducer::replay_store(&store).unwrap();
    let candidate = &state.candidates[&candidate_id];
    assert_eq!(candidate.authorizer, "authorizer");
    assert_eq!(candidate.phase.as_str(), "landed_out_of_band");
    assert_eq!(candidate.phase_op_id, reconciliation_op);
    assert_eq!(
        candidate.reviews["reviewer"].verdict,
        ReviewVerdict::Approve
    );
    assert_eq!(
        candidate.authorization.as_ref().unwrap().op_id,
        authorization_op
    );
    assert_eq!(
        candidate.authorization.as_ref().unwrap().status,
        AuthorizationStatus::Granted
    );
    let recorded = candidate.reconciled.as_ref().unwrap();
    assert_eq!(
        recorded.authority,
        CandidateReconciliationAuthority::ExplicitOperatorOverride
    );
    assert_eq!(recorded.actor, "recovery-operator");
    assert_eq!(recorded.policy_snapshot, current_snapshot);
    let basis = recorded.override_basis.as_ref().unwrap();
    assert_eq!(basis.expect_authorizer, "authorizer");
    assert_eq!(basis.expect_landing_repository_id, REPO);
    assert_eq!(
        basis.authority_refs,
        vec!["appointment-ack", "appointment-record"]
    );
    assert!(basis.reason.contains("proposal authorizer is unavailable"));
    assert!(recorded.repository_bridge.is_none());
}

#[test]
fn abandoned_reconciliation_requires_an_exact_audited_override_and_preserves_history() {
    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &proposal(
            &candidate_id,
            &issue,
            COMMIT_A,
            "abandoned-reconciliation-proposal",
        ),
    );
    let abandonment_op = abandon(&store, &candidate_id, "abandoned-reconciliation-abandon");

    let (mut mislabeled_reachability, _) = reachability_evidence(
        &candidate_id,
        "recovery-operator",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "abandoned-reconciliation-mislabeled-reachability",
    );
    let Op::CandidateEvidence(mislabeled_record) = &mut mislabeled_reachability else {
        unreachable!()
    };
    mislabeled_record.name = "external-tests".into();
    publish_rejected(
        &store,
        &mislabeled_reachability,
        "terminal candidates accept only built-in Git ancestry bookkeeping",
    );

    let (bystander_evidence, bystander_evidence_id) = reachability_evidence(
        &candidate_id,
        "bystander",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "abandoned-reconciliation-bystander-reachability",
    );
    publish_checked(&store, &bystander_evidence);
    let bystander_snapshot = reducer::replay_store(&store)
        .unwrap()
        .candidate_policy_snapshot(&candidate_id)
        .unwrap();
    publish_rejected(
        &store,
        &override_reconcile(
            &candidate_id,
            "recovery-operator",
            &bystander_evidence_id,
            &abandonment_op,
            "authorizer",
            vec!["legacy-abandonment-record"],
            bystander_snapshot,
            "abandoned-reconciliation-bystander-receipt",
        ),
        "reconciliation reachability evidence not found",
    );

    let (failed_evidence, failed_evidence_id) = reachability_evidence(
        &candidate_id,
        "recovery-operator",
        EvidenceOutcome::Fail,
        Some(false),
        REPO,
        "abandoned-reconciliation-failed-reachability",
    );
    publish_checked(&store, &failed_evidence);
    let failed_snapshot = reducer::replay_store(&store)
        .unwrap()
        .candidate_policy_snapshot(&candidate_id)
        .unwrap();
    publish_rejected(
        &store,
        &override_reconcile(
            &candidate_id,
            "recovery-operator",
            &failed_evidence_id,
            &abandonment_op,
            "authorizer",
            vec!["legacy-abandonment-record"],
            failed_snapshot,
            "abandoned-reconciliation-failed-receipt",
        ),
        "passing exact-reachability receipt",
    );

    let (passing_evidence, passing_evidence_id) = reachability_evidence(
        &candidate_id,
        "recovery-operator",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "abandoned-reconciliation-passing-reachability",
    );
    publish_checked(&store, &passing_evidence);
    let (authorizer_evidence, authorizer_evidence_id) = reachability_evidence(
        &candidate_id,
        "authorizer",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        "abandoned-reconciliation-authorizer-reachability",
    );
    publish_checked(&store, &authorizer_evidence);
    let current_snapshot = reducer::replay_store(&store)
        .unwrap()
        .candidate_policy_snapshot(&candidate_id)
        .unwrap();

    publish_rejected(
        &store,
        &reconcile(
            &candidate_id,
            "authorizer",
            &authorizer_evidence_id,
            &abandonment_op,
            current_snapshot.clone(),
            "abandoned-reconciliation-ordinary-authorizer",
        ),
        "explicit operator override",
    );

    publish_rejected(
        &store,
        &override_reconcile(
            &candidate_id,
            "recovery-operator",
            &passing_evidence_id,
            &proposal_op,
            "authorizer",
            vec!["legacy-abandonment-record"],
            current_snapshot.clone(),
            "abandoned-reconciliation-stale-phase",
        ),
        "current pending phase CAS",
    );

    let mut missing_basis = override_reconcile(
        &candidate_id,
        "recovery-operator",
        &passing_evidence_id,
        &abandonment_op,
        "authorizer",
        vec!["legacy-abandonment-record"],
        current_snapshot.clone(),
        "abandoned-reconciliation-missing-basis",
    );
    let Op::CandidateReconcile(operation) = &mut missing_basis else {
        unreachable!()
    };
    operation.override_basis = None;
    publish_rejected(&store, &missing_basis, "exact original-authorizer CAS");

    let reconciliation_op = publish_checked(
        &store,
        &override_reconcile(
            &candidate_id,
            "recovery-operator",
            &passing_evidence_id,
            &abandonment_op,
            "authorizer",
            vec!["legacy-abandonment-record"],
            current_snapshot,
            "abandoned-reconciliation-success",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    let candidate = &state.candidates[&candidate_id];
    assert_eq!(candidate.phase.as_str(), "landed_out_of_band");
    assert_eq!(candidate.phase_op_id, reconciliation_op);
    assert_eq!(
        candidate.reconciled.as_ref().unwrap().authority,
        CandidateReconciliationAuthority::ExplicitOperatorOverride
    );
    assert!(state.was_accepted(&abandonment_op));
    assert!(
        store
            .ops_dir()
            .join(format!("{abandonment_op}.json"))
            .is_file(),
        "reconciliation must append a new fact without erasing the accepted abandonment"
    );

    let superseded = ids::new_candidate_id();
    let successor = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(
            &superseded,
            &issue,
            COMMIT_B,
            "superseded-reconciliation-predecessor",
        ),
    );
    publish_checked(
        &store,
        &proposal(
            &successor,
            &issue,
            COMMIT_C,
            "superseded-reconciliation-successor",
        ),
    );
    let (superseded_evidence, superseded_evidence_id) = reachability_evidence_for_commit(
        &superseded,
        "recovery-operator",
        EvidenceOutcome::Pass,
        Some(true),
        REPO,
        COMMIT_B,
        "superseded-reconciliation-reachability",
    );
    publish_checked(&store, &superseded_evidence);
    let pending = reducer::replay_store(&store).unwrap();
    publish_checked(
        &store,
        &Op::CandidateSupersede(CandidateSupersedeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: superseded.clone(),
            successor_id: successor,
            expect_phase: pending.candidates[&superseded].phase_op_id.clone(),
            recovery: None,
            idempotency_key: "superseded-reconciliation-transition".into(),
        }),
    );
    let superseded_phase = reducer::replay_store(&store).unwrap().candidates[&superseded]
        .phase_op_id
        .clone();
    let superseded_snapshot = reducer::replay_store(&store)
        .unwrap()
        .candidate_policy_snapshot(&superseded)
        .unwrap();
    publish_rejected(
        &store,
        &override_reconcile(
            &superseded,
            "recovery-operator",
            &superseded_evidence_id,
            &superseded_phase,
            "authorizer",
            vec!["legacy-abandonment-record"],
            superseded_snapshot,
            "superseded-reconciliation-refusal",
        ),
        "current pending phase CAS",
    );
}

#[test]
fn cross_repository_override_requires_a_current_exact_object_bridge() {
    const OBSERVED_REPO: &str = "repo-observed-store";

    let (_temp, store, issue) = setup();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &store,
        &proposal(
            &candidate_id,
            &issue,
            COMMIT_A,
            "cross-repository-override-proposal",
        ),
    );
    let (mut reachability, evidence_id) = reachability_evidence(
        &candidate_id,
        "recovery-operator",
        EvidenceOutcome::Pass,
        Some(true),
        OBSERVED_REPO,
        "cross-repository-override-reachability",
    );
    let Op::CandidateEvidence(reachability) = &mut reachability else {
        unreachable!()
    };
    reachability.name = GIT_RECONCILIATION_REACHABILITY_EVIDENCE.into();
    publish_checked(&store, &Op::CandidateEvidence(reachability.clone()));

    let snapshot = reducer::replay_store(&store)
        .unwrap()
        .candidate_policy_snapshot(&candidate_id)
        .unwrap();
    let operation = |key: &str| {
        let mut operation = override_reconcile(
            &candidate_id,
            "recovery-operator",
            &evidence_id,
            &proposal_op,
            "authorizer",
            vec!["appointment-record"],
            snapshot.clone(),
            key,
        );
        let Op::CandidateReconcile(reconcile) = &mut operation else {
            unreachable!()
        };
        reconcile.repository_bridge = Some(CandidateReconciliationRepositoryBridge {
            expect_landing_repository_op_id: proposal_op.clone(),
            object_availability: GitObjectAvailabilityReceipt {
                repository_id: OBSERVED_REPO.into(),
                object_format: "sha1".into(),
                candidate_oid: COMMIT_A.into(),
                observed_parent_oids: vec![BASE.into()],
                object_available: Some(true),
                git_version: "git version test".into(),
                detail: None,
            },
        });
        operation
    };

    let mut missing_bridge = operation("cross-repository-missing-bridge");
    let Op::CandidateReconcile(reconcile) = &mut missing_bridge else {
        unreachable!()
    };
    reconcile.repository_bridge = None;
    publish_rejected(
        &store,
        &missing_bridge,
        "exact-reachability receipt in the bound repository",
    );

    let mut stale_binding = operation("cross-repository-stale-binding");
    let Op::CandidateReconcile(reconcile) = &mut stale_binding else {
        unreachable!()
    };
    reconcile
        .repository_bridge
        .as_mut()
        .unwrap()
        .expect_landing_repository_op_id = "op-stale-binding".into();
    publish_rejected(&store, &stale_binding, "exact current binding");

    let mut wrong_parent = operation("cross-repository-wrong-parent");
    let Op::CandidateReconcile(reconcile) = &mut wrong_parent else {
        unreachable!()
    };
    reconcile
        .repository_bridge
        .as_mut()
        .unwrap()
        .object_availability
        .observed_parent_oids = vec![COMMIT_B.into()];
    publish_rejected(&store, &wrong_parent, "parent");

    let mut mismatched_repository = operation("cross-repository-mismatched-repository");
    let Op::CandidateReconcile(reconcile) = &mut mismatched_repository else {
        unreachable!()
    };
    reconcile
        .repository_bridge
        .as_mut()
        .unwrap()
        .object_availability
        .repository_id = "repo-other-observation".into();
    publish_rejected(
        &store,
        &mismatched_repository,
        "same repository and Git tool",
    );

    let mut mismatched_tool = operation("cross-repository-mismatched-tool");
    let Op::CandidateReconcile(reconcile) = &mut mismatched_tool else {
        unreachable!()
    };
    reconcile
        .repository_bridge
        .as_mut()
        .unwrap()
        .object_availability
        .git_version = "git version caller-selected".into();
    publish_rejected(&store, &mismatched_tool, "same repository and Git tool");

    let mut authorizer_bridge = operation("cross-repository-authorizer-bridge");
    let Op::CandidateReconcile(reconcile) = &mut authorizer_bridge else {
        unreachable!()
    };
    reconcile.actor = "authorizer".into();
    reconcile.authority = CandidateReconciliationAuthority::ProposalAuthorizer;
    reconcile.override_basis = None;
    publish_rejected(&store, &authorizer_bridge, "explicit operator override");

    let reconciliation_op = publish_checked(&store, &operation("cross-repository-success"));
    let state = reducer::replay_store(&store).unwrap();
    let candidate = &state.candidates[&candidate_id];
    assert_eq!(candidate.repository_id, REPO);
    assert_eq!(candidate.landing_repository_id, REPO);
    assert!(candidate.landing_repository_bindings.is_empty());
    assert_eq!(candidate.phase_op_id, reconciliation_op);
    let reconciliation = candidate.reconciled.as_ref().unwrap();
    assert_eq!(
        reconciliation
            .repository_bridge
            .as_ref()
            .unwrap()
            .object_availability
            .repository_id,
        OBSERVED_REPO
    );
}

#[test]
fn reachability_probe_fails_closed_for_identity_ref_and_object_gaps() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(temp.path(), &["config", "user.email", "test@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Test"]);
    std::fs::write(temp.path().join("work.txt"), "root\n").unwrap();
    run_git(temp.path(), &["add", "work.txt"]);
    run_git(temp.path(), &["commit", "-qm", "root"]);
    let head = run_git(temp.path(), &["rev-parse", "HEAD"]);
    let ancestry = mote::candidate::probe_ancestry(temp.path(), &head, &head, &[]).unwrap();

    assert!(
        mote::candidate::probe_reachability(
            temp.path(),
            "repo-wrong",
            &ancestry.object_format,
            &head,
            "HEAD",
        )
        .unwrap_err()
        .contains("identity")
    );
    assert!(
        mote::candidate::probe_reachability(
            temp.path(),
            &ancestry.repository_id,
            &ancestry.object_format,
            &head,
            "refs/heads/missing",
        )
        .is_err()
    );
    let missing_object = "ffffffffffffffffffffffffffffffffffffffff";
    let receipt = mote::candidate::probe_reachability(
        temp.path(),
        &ancestry.repository_id,
        &ancestry.object_format,
        missing_object,
        "HEAD",
    )
    .unwrap();
    assert_eq!(receipt.candidate_reachable, None);
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
                    base_oid: None,
                    base_relation: Some(GitRelationKind::NotAncestor),
                    relation: GitRelationKind::Ancestor,
                    subject_to_known_base: None,
                    subject_to_known_tip: None,
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
            recovery: None,
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
    assert_eq!(
        state.candidates[&old]
            .supersession
            .as_ref()
            .unwrap()
            .authority,
        CandidateSupersessionAuthority::PredecessorProposer
    );
}

#[test]
fn superseded_candidate_ancestry_has_a_fail_closed_audited_refresh_path() {
    let (temp, store, issue) = setup();
    let target = ids::new_candidate_id();
    let predecessor = ids::new_candidate_id();
    let successor = ids::new_candidate_id();

    let target_proposal = publish_checked(
        &store,
        &proposal(&target, &issue, COMMIT_C, "override-target-proposal"),
    );
    publish_checked(
        &store,
        &evidence(
            &target,
            ancestry_payload(&target, COMMIT_C, Vec::new(), Vec::new()),
            "override-target-ancestry",
        ),
    );
    publish_checked(&store, &approve(&target, "override-target-review"));
    publish_checked(&store, &authorize(&target, "override-target-authorize"));

    let predecessor_proposal = publish_checked(
        &store,
        &proposal(
            &predecessor,
            &issue,
            COMMIT_A,
            "override-predecessor-proposal",
        ),
    );
    let predecessor_evidence_op = publish_checked(
        &store,
        &evidence(
            &predecessor,
            ancestry_payload(&predecessor, COMMIT_A, Vec::new(), Vec::new()),
            "override-predecessor-ancestry",
        ),
    );

    let successor_proposal = publish_checked(
        &store,
        &proposal(&successor, &issue, COMMIT_B, "override-successor-proposal"),
    );
    publish_checked(
        &store,
        &evidence(
            &successor,
            v2_ancestry_payload(
                COMMIT_B,
                vec![
                    (target.clone(), target_proposal.clone()),
                    (predecessor.clone(), predecessor_proposal.clone()),
                ],
                vec![
                    v2_relation(
                        &target,
                        &target_proposal,
                        COMMIT_C,
                        GitRelationKind::NotAncestor,
                        GitRelationKind::NotAncestor,
                        GitRelationKind::NotAncestor,
                        GitRelationKind::NotAncestor,
                    ),
                    v2_relation(
                        &predecessor,
                        &predecessor_proposal,
                        COMMIT_A,
                        GitRelationKind::NotAncestor,
                        GitRelationKind::NotAncestor,
                        GitRelationKind::NotAncestor,
                        GitRelationKind::NotAncestor,
                    ),
                ],
                "override-successor-snapshot",
            ),
            "override-successor-ancestry",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    publish_checked(
        &store,
        &Op::CandidateSupersede(CandidateSupersedeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: predecessor.clone(),
            successor_id: successor.clone(),
            expect_phase: state.candidates[&predecessor].phase_op_id.clone(),
            recovery: None,
            idempotency_key: "override-supersede-predecessor".into(),
        }),
    );

    let state = reducer::replay_store(&store).unwrap();
    let blocked = state.candidate_landability(&target, Some("lander"));
    assert!(
        blocked.reason_codes.contains(&"git_evidence_stale".into()),
        "superseded candidates must remain pairwise visible: {blocked:?}"
    );
    let predecessor_phase = state.candidates[&predecessor].phase_op_id.clone();

    let refreshed_payload = v2_ancestry_payload(
        COMMIT_A,
        vec![
            (target.clone(), target_proposal.clone()),
            (successor.clone(), successor_proposal.clone()),
        ],
        vec![
            v2_relation(
                &target,
                &target_proposal,
                COMMIT_C,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
            ),
            v2_relation(
                &successor,
                &successor_proposal,
                COMMIT_B,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
            ),
        ],
        "override-predecessor-refresh-snapshot",
    );
    let CandidateEvidencePayload::GitAncestry(refreshed_receipt) = refreshed_payload else {
        panic!("expected ordinary ancestry payload")
    };
    let basis = CandidateEvidenceOperatorOverride {
        expect_phase_op_id: predecessor_phase.clone(),
        expect_authorizer: "authorizer".into(),
        expect_landing_repository_id: REPO.into(),
        expect_producers: vec!["proposer".into()],
        expect_producer_evidence_op_ids: vec![predecessor_evidence_op.clone()],
        reason: "immutable producer is unavailable".into(),
        authority_refs: vec!["board:coordination/post-recovery".into()],
    };

    let mut mutations = Vec::new();
    let mut wrong_phase = basis.clone();
    wrong_phase.expect_phase_op_id = "stale-phase".into();
    mutations.push((
        "wrong-phase",
        wrong_phase,
        basis.authority_refs.clone(),
        "git version test",
    ));
    let mut wrong_authorizer = basis.clone();
    wrong_authorizer.expect_authorizer = "someone-else".into();
    mutations.push((
        "wrong-authorizer",
        wrong_authorizer,
        basis.authority_refs.clone(),
        "git version test",
    ));
    let mut wrong_repository = basis.clone();
    wrong_repository.expect_landing_repository_id = "repo-other".into();
    mutations.push((
        "wrong-repository",
        wrong_repository,
        basis.authority_refs.clone(),
        "git version test",
    ));
    let mut wrong_producers = basis.clone();
    wrong_producers.expect_producers = vec!["departed-producer".into()];
    mutations.push((
        "wrong-producers",
        wrong_producers,
        basis.authority_refs.clone(),
        "git version test",
    ));
    let mut wrong_prior_evidence = basis.clone();
    wrong_prior_evidence.expect_producer_evidence_op_ids = vec!["stale-evidence-op".into()];
    mutations.push((
        "wrong-prior-evidence",
        wrong_prior_evidence,
        basis.authority_refs.clone(),
        "git version test",
    ));
    let mut empty_reason = basis.clone();
    empty_reason.reason.clear();
    mutations.push((
        "empty-reason",
        empty_reason,
        basis.authority_refs.clone(),
        "git version test",
    ));
    let mut empty_refs = basis.clone();
    empty_refs.authority_refs.clear();
    mutations.push(("empty-refs", empty_refs, Vec::new(), "git version test"));
    mutations.push((
        "mismatched-refs",
        basis.clone(),
        vec!["board:other".into()],
        "git version test",
    ));
    mutations.push((
        "wrong-tool",
        basis.clone(),
        basis.authority_refs.clone(),
        "git version forged",
    ));

    for (label, mutated_basis, refs, producer_tool) in mutations {
        let mut mutated = operator_ancestry_evidence(
            &predecessor,
            refreshed_receipt.clone(),
            mutated_basis,
            "authorizer",
            refs,
            &format!("override-mutation-{label}"),
        );
        let Op::CandidateEvidence(mutated_record) = &mut mutated else {
            unreachable!()
        };
        mutated_record.producer_tool = producer_tool.into();
        publish_rejected(&store, &mutated, "operator ancestry refresh requires exact");
    }
    publish_rejected(
        &store,
        &operator_ancestry_evidence(
            &predecessor,
            refreshed_receipt.clone(),
            basis.clone(),
            "proposer",
            basis.authority_refs.clone(),
            "override-named-producer",
        ),
        "operator ancestry refresh requires exact",
    );

    let ordinary_payload = CandidateEvidencePayload::GitAncestry(refreshed_receipt.clone());
    let ordinary_unauthorized = Op::CandidateEvidence(CandidateEvidenceOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "operator".into(),
        candidate_id: predecessor.clone(),
        candidate_oid: COMMIT_A.into(),
        evidence_id: evidence_id(&ordinary_payload).unwrap(),
        name: GIT_ANCESTRY_EVIDENCE.into(),
        evidence_kind: "git".into(),
        producer_tool: "git version test".into(),
        outcome: EvidenceOutcome::Pass,
        payload: ordinary_payload,
        refs: Vec::new(),
        idempotency_key: "override-ordinary-unauthorized".into(),
    });
    publish_rejected(&store, &ordinary_unauthorized, "is not a named producer");
    let mut wrong_evidence_class = operator_ancestry_evidence(
        &predecessor,
        refreshed_receipt.clone(),
        basis.clone(),
        "authorizer",
        basis.authority_refs.clone(),
        "override-wrong-evidence-class",
    );
    let Op::CandidateEvidence(wrong_evidence_record) = &mut wrong_evidence_class else {
        unreachable!()
    };
    wrong_evidence_record.name = "external-tests".into();
    publish_rejected(
        &store,
        &wrong_evidence_class,
        "terminal candidates accept only built-in Git ancestry bookkeeping",
    );

    publish_checked(
        &store,
        &operator_ancestry_evidence(
            &predecessor,
            refreshed_receipt.clone(),
            basis.clone(),
            "authorizer",
            basis.authority_refs.clone(),
            "override-valid-refresh",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.candidates[&predecessor].phase,
        mote::candidate::CandidatePhase::Superseded
    );
    assert!(
        state
            .candidate_landability(&target, Some("lander"))
            .landable
    );

    publish_checked(
        &store,
        &evidence(
            &predecessor,
            CandidateEvidencePayload::GitAncestry(refreshed_receipt),
            "terminal-named-producer-refresh",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state.candidates[&predecessor]
            .evidence
            .values()
            .any(|record| record.payload.git_ancestry_override().is_some())
    );

    let shown = run_mote(temp.path(), &["candidate", "show", &predecessor]);
    assert!(shown.status.success());
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(shown.contains("ancestry refresh override:"), "{shown}");
    assert!(
        shown.contains("immutable producer is unavailable"),
        "{shown}"
    );

    let audit = run_mote(temp.path(), &["--json", "audit", "--fail-on", "never"]);
    assert!(audit.status.success());
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    let finding = audit["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "candidate_ancestry_operator_override_recorded")
        .unwrap();
    assert_eq!(finding["subject"], predecessor);
    assert_eq!(
        finding["evidence"]["expected_phase_op_id"],
        predecessor_phase
    );
    assert_eq!(
        finding["evidence"]["expected_producer_evidence_op_ids"][0],
        predecessor_evidence_op
    );
}

#[test]
fn ordinary_supersession_still_accepts_the_predecessor_authorizer() {
    let (_temp, store, issue) = setup();
    let predecessor = ids::new_candidate_id();
    let successor = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(
            &predecessor,
            &issue,
            COMMIT_A,
            "ordinary-authorizer-predecessor",
        ),
    );
    publish_checked(
        &store,
        &proposal(
            &successor,
            &issue,
            COMMIT_B,
            "ordinary-authorizer-successor",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    publish_checked(
        &store,
        &Op::CandidateSupersede(CandidateSupersedeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: predecessor.clone(),
            successor_id: successor,
            expect_phase: state.candidates[&predecessor].phase_op_id.clone(),
            recovery: None,
            idempotency_key: "ordinary-authorizer-supersede".into(),
        }),
    );
    let state = reducer::replay_store(&store).unwrap();
    let record = state.candidates[&predecessor]
        .supersession
        .as_ref()
        .unwrap();
    assert_eq!(
        record.authority,
        CandidateSupersessionAuthority::PredecessorAuthorizer
    );
    assert!(record.containment_evidence_op_ids.is_empty());
}

#[test]
fn successor_authorizer_can_recover_only_with_exact_complete_containment_evidence() {
    let (temp, store, issue) = setup();
    let predecessor = ids::new_candidate_id();
    let successor = ids::new_candidate_id();
    let predecessor_proposal = publish_checked(
        &store,
        &proposal_with_authorizer(
            &predecessor,
            &issue,
            COMMIT_A,
            "predecessor-authorizer",
            "recovery-predecessor",
        ),
    );
    publish_checked(
        &store,
        &proposal_with_authorizer(
            &successor,
            &issue,
            COMMIT_B,
            "successor-authorizer",
            "recovery-successor",
        ),
    );

    let complete_payload = |digest: &str| {
        v2_ancestry_payload(
            COMMIT_B,
            vec![(predecessor.clone(), predecessor_proposal.clone())],
            vec![v2_relation(
                &predecessor,
                &predecessor_proposal,
                COMMIT_A,
                GitRelationKind::NotAncestor,
                GitRelationKind::Ancestor,
                GitRelationKind::NotAncestor,
                GitRelationKind::NotAncestor,
            )],
            digest,
        )
    };
    let initial_evidence = publish_checked(
        &store,
        &evidence(
            &successor,
            complete_payload("recovery-initial-complete"),
            "recovery-initial-evidence",
        ),
    );
    publish_checked(&store, &approve(&successor, "recovery-review-successor"));
    publish_checked(
        &store,
        &authorize_as(
            &successor,
            "successor-authorizer",
            "recovery-authorize-successor",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    let predecessor_phase = state.candidates[&predecessor].phase_op_id.clone();
    let successor_phase = state.candidates[&successor].phase_op_id.clone();
    let initial_basis = state
        .candidate_containment_basis(&predecessor, &successor)
        .unwrap();
    assert_eq!(
        initial_basis.evidence_op_ids.as_slice(),
        std::slice::from_ref(&initial_evidence)
    );
    assert!(
        state
            .candidate_landability(&successor, Some("lander"))
            .reason_codes
            .contains(&"ancestor_pending".into())
    );

    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "intruder",
            &predecessor_phase,
            &successor_phase,
            vec![initial_evidence.clone()],
            CandidateSupersessionAuthority::SuccessorAuthorizerContainment,
            "recovery-intruder",
        ),
        "immutable successor authorizer",
    );
    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "successor-authorizer",
            &predecessor_phase,
            &successor_phase,
            vec![initial_evidence.clone()],
            CandidateSupersessionAuthority::PredecessorProposer,
            "recovery-false-authority",
        ),
        "immutable successor authorizer",
    );
    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "successor-authorizer",
            &predecessor_phase,
            "op-stale-successor-phase",
            vec![initial_evidence.clone()],
            CandidateSupersessionAuthority::SuccessorAuthorizerContainment,
            "recovery-stale-successor",
        ),
        "current successor phase CAS",
    );
    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "successor-authorizer",
            &predecessor_phase,
            &successor_phase,
            vec!["op-stale-evidence".into()],
            CandidateSupersessionAuthority::SuccessorAuthorizerContainment,
            "recovery-stale-evidence",
        ),
        "stale containment evidence CAS",
    );

    let not_contained = publish_checked(
        &store,
        &evidence(
            &successor,
            v2_ancestry_payload(
                COMMIT_B,
                vec![(predecessor.clone(), predecessor_proposal.clone())],
                vec![v2_relation(
                    &predecessor,
                    &predecessor_proposal,
                    COMMIT_A,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                )],
                "recovery-not-contained",
            ),
            "recovery-not-contained-evidence",
        ),
    );
    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "successor-authorizer",
            &predecessor_phase,
            &successor_phase,
            vec![not_contained],
            CandidateSupersessionAuthority::SuccessorAuthorizerContainment,
            "recovery-not-contained-attempt",
        ),
        "predecessor commit is an ancestor",
    );

    let partial = publish_checked(
        &store,
        &evidence(
            &successor,
            v2_ancestry_payload(
                COMMIT_B,
                vec![(predecessor.clone(), predecessor_proposal.clone())],
                vec![v2_relation(
                    &predecessor,
                    &predecessor_proposal,
                    COMMIT_A,
                    GitRelationKind::Unavailable,
                    GitRelationKind::Ancestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                )],
                "recovery-partial",
            ),
            "recovery-partial-evidence",
        ),
    );
    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "successor-authorizer",
            &predecessor_phase,
            &successor_phase,
            vec![partial],
            CandidateSupersessionAuthority::SuccessorAuthorizerContainment,
            "recovery-partial-attempt",
        ),
        "complete determinate evidence",
    );

    let final_evidence = publish_checked(
        &store,
        &evidence(
            &successor,
            complete_payload("recovery-final-complete"),
            "recovery-final-evidence",
        ),
    );
    let args = [
        "--json",
        "--actor",
        "successor-authorizer",
        "candidate",
        "supersede",
        &predecessor,
        &successor,
        "--expect-phase",
        &predecessor_phase,
        "--containment-recovery",
        "--idempotency-key",
        "recovery-cli-success",
    ];
    let recovered = run_mote(temp.path(), &args);
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let recovered_json: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(recovered_json["phase"]["value"], "superseded");
    assert_eq!(
        recovered_json["supersession"]["record"]["authority"],
        "successor_authorizer_containment"
    );
    assert_eq!(
        recovered_json["supersession"]["record"]["containment_evidence_op_ids"][0],
        final_evidence
    );
    let shown = run_mote(temp.path(), &["candidate", "show", &predecessor]);
    assert!(shown.status.success());
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(
        shown.contains("authority=successor_authorizer_containment"),
        "{shown}"
    );

    let state = reducer::replay_store(&store).unwrap();
    let record = state.candidates[&predecessor]
        .supersession
        .as_ref()
        .unwrap();
    assert_eq!(record.actor, "successor-authorizer");
    assert_eq!(
        record.authority,
        CandidateSupersessionAuthority::SuccessorAuthorizerContainment
    );
    assert_eq!(record.containment_evidence_op_ids, [final_evidence]);
    assert_eq!(record.op_id, state.candidates[&predecessor].phase_op_id);
    assert!(
        state
            .candidate_landability(&successor, Some("lander"))
            .landable
    );
    assert_eq!(
        format!("{:?}", reducer::replay_store(&store).unwrap()),
        format!("{:?}", state)
    );

    let audit = run_mote(temp.path(), &["--json", "audit", "--fail-on", "never"]);
    assert!(audit.status.success());
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    assert!(audit["findings"].as_array().unwrap().iter().any(|finding| {
        finding["code"] == "candidate_containment_recovery_recorded"
            && finding["subject"] == predecessor
    }));

    let op_count_before_retry = store.list_op_filenames().unwrap().len();
    let retry = run_mote(temp.path(), &args);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_before_retry,
        "recovery retry must return the accepted action without publishing a new snapshot"
    );
    let accepted_phase = reducer::replay_store(&store).unwrap().candidates[&predecessor]
        .phase_op_id
        .clone();
    assert_eq!(accepted_phase, record.op_id);

    publish_rejected(
        &store,
        &containment_recovery(
            &predecessor,
            &successor,
            "successor-authorizer",
            &predecessor_phase,
            &successor_phase,
            record.containment_evidence_op_ids.clone(),
            CandidateSupersessionAuthority::SuccessorAuthorizerContainment,
            "recovery-after-terminal-race",
        ),
        "current predecessor phase CAS",
    );
}

#[test]
fn newer_v2_receipt_resolves_an_unrelated_older_candidate_without_refreshing_it() {
    let (_temp, store, issue) = setup();
    let old = ids::new_candidate_id();
    let new = ids::new_candidate_id();
    let _old_proposal = publish_checked(
        &store,
        &proposal(&old, &issue, COMMIT_A, "propose-old-unrelated"),
    );
    publish_checked(
        &store,
        &evidence(
            &old,
            ancestry_payload(&old, COMMIT_A, Vec::new(), Vec::new()),
            "ancestry-old-unrelated",
        ),
    );
    publish_checked(&store, &approve(&old, "review-old-unrelated"));
    publish_checked(&store, &authorize(&old, "authorize-old-unrelated"));
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_landability(&old, Some("lander"))
            .landable
    );

    let new_proposal = publish_checked(
        &store,
        &proposal(&new, &issue, COMMIT_B, "propose-new-unrelated"),
    );
    let stale = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&old, Some("lander"));
    assert!(stale.reason_codes.contains(&"git_evidence_stale".into()));

    let old_proposal = reducer::replay_store(&store).unwrap().candidates[&old]
        .proposal_op_id
        .clone();
    publish_checked(
        &store,
        &evidence(
            &new,
            v2_ancestry_payload(
                COMMIT_B,
                vec![(old.clone(), old_proposal.clone())],
                vec![v2_relation(
                    &old,
                    &old_proposal,
                    COMMIT_A,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                )],
                "snapshot-new-unrelated",
            ),
            "ancestry-new-unrelated",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.candidates[&new].proposal_op_id, new_proposal);
    let restored = state.candidate_landability(&old, Some("lander"));
    assert!(restored.landable, "{restored:?}");
}

#[test]
fn reciprocal_v2_evidence_with_the_wrong_known_base_cannot_resolve_a_pair() {
    let (_temp, store, issue) = setup();
    let old = ids::new_candidate_id();
    let new = ids::new_candidate_id();
    let old_proposal = publish_checked(
        &store,
        &proposal(&old, &issue, COMMIT_A, "propose-anchor-old"),
    );
    publish_checked(
        &store,
        &evidence(
            &old,
            ancestry_payload(&old, COMMIT_A, Vec::new(), Vec::new()),
            "ancestry-anchor-old",
        ),
    );
    publish_checked(
        &store,
        &proposal(&new, &issue, COMMIT_B, "propose-anchor-new"),
    );
    let mut wrong_anchor = v2_relation(
        &old,
        &old_proposal,
        COMMIT_A,
        GitRelationKind::NotAncestor,
        GitRelationKind::NotAncestor,
        GitRelationKind::NotAncestor,
        GitRelationKind::NotAncestor,
    );
    wrong_anchor.base_oid = Some(COMMIT_B.into());
    publish_checked(
        &store,
        &evidence(
            &new,
            v2_ancestry_payload(
                COMMIT_B,
                vec![(old.clone(), old_proposal)],
                vec![wrong_anchor],
                "snapshot-wrong-known-base",
            ),
            "ancestry-wrong-known-base",
        ),
    );

    let landability = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&old, Some("lander"));
    assert!(
        landability
            .reason_codes
            .contains(&"git_evidence_stale".into()),
        "{landability:?}"
    );
}

#[test]
fn newer_v2_receipt_exposes_a_hidden_ancestor_of_an_older_candidate() {
    let (_temp, store, issue) = setup();
    let old = ids::new_candidate_id();
    let new_ancestor = ids::new_candidate_id();
    let old_proposal = publish_checked(
        &store,
        &proposal(&old, &issue, COMMIT_B, "propose-old-descendant"),
    );
    publish_checked(
        &store,
        &evidence(
            &old,
            ancestry_payload(&old, COMMIT_B, Vec::new(), Vec::new()),
            "ancestry-old-descendant",
        ),
    );
    publish_checked(&store, &approve(&old, "review-old-descendant"));
    publish_checked(&store, &authorize(&old, "authorize-old-descendant"));
    publish_checked(
        &store,
        &proposal(&new_ancestor, &issue, COMMIT_A, "propose-later-ancestor"),
    );
    publish_checked(
        &store,
        &evidence(
            &new_ancestor,
            v2_ancestry_payload(
                COMMIT_A,
                vec![(old.clone(), old_proposal.clone())],
                vec![v2_relation(
                    &old,
                    &old_proposal,
                    COMMIT_B,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::Ancestor,
                )],
                "snapshot-later-ancestor",
            ),
            "ancestry-later-ancestor",
        ),
    );

    let landability = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&old, Some("lander"));
    assert!(
        landability
            .reason_codes
            .contains(&"ancestor_pending".into()),
        "{landability:?}"
    );
    assert!(
        !landability
            .reason_codes
            .contains(&"git_evidence_stale".into())
    );
}

#[test]
fn concurrent_v2_snapshots_that_omit_each_other_remain_stale() {
    let (temp, store, issue) = setup();
    let first = ids::new_candidate_id();
    let second = ids::new_candidate_id();
    let first_proposal = publish_checked(
        &store,
        &proposal(&first, &issue, COMMIT_A, "propose-concurrent-first"),
    );
    publish_checked(
        &store,
        &evidence(
            &first,
            v2_ancestry_payload(COMMIT_A, Vec::new(), Vec::new(), "snapshot-first-empty"),
            "ancestry-concurrent-first",
        ),
    );
    let second_proposal = publish_checked(
        &store,
        &proposal(&second, &issue, COMMIT_B, "propose-concurrent-second"),
    );
    publish_checked(
        &store,
        &evidence(
            &second,
            v2_ancestry_payload(COMMIT_B, Vec::new(), Vec::new(), "snapshot-second-empty"),
            "ancestry-concurrent-second",
        ),
    );

    let state = reducer::replay_store(&store).unwrap();
    for candidate_id in [&first, &second] {
        let landability = state.candidate_landability(candidate_id, Some("lander"));
        assert!(
            landability
                .reason_codes
                .contains(&"git_evidence_stale".into()),
            "{candidate_id}: {landability:?}"
        );
    }
    let first_landability = state.candidate_landability(&first, Some("lander"));
    let stale_detail = &first_landability
        .reasons
        .iter()
        .find(|reason| reason.code == "git_evidence_stale")
        .unwrap()
        .detail;
    assert!(
        stale_detail.contains(&format!("predates proposal op {second_proposal}")),
        "{stale_detail}"
    );
    assert!(
        stale_detail.contains(&format!(
            "proposal op {first_proposal} is absent from the declared producer snapshot"
        )),
        "{stale_detail}"
    );
    assert!(
        stale_detail
            .contains("repeating the refresh in this producer checkout cannot repair the target"),
        "{stale_detail}"
    );
    assert!(
        stale_detail.contains("refresh from a store snapshot containing both exact proposal ops"),
        "{stale_detail}"
    );

    let shown = run_mote(temp.path(), &["candidate", "show", &first]);
    assert!(shown.status.success());
    let shown = String::from_utf8(shown.stdout).unwrap();
    assert!(shown.contains("git_evidence_stale"), "{shown}");
    assert!(
        shown.contains("absent from the declared producer snapshot"),
        "{shown}"
    );
}

#[test]
fn stale_pair_diagnostic_falls_back_honestly_for_legacy_receipts() {
    let (_temp, store, issue) = setup();
    let first = ids::new_candidate_id();
    let second = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(&first, &issue, COMMIT_A, "propose-legacy-first"),
    );
    publish_checked(
        &store,
        &evidence(
            &first,
            ancestry_payload(&first, COMMIT_A, Vec::new(), Vec::new()),
            "ancestry-legacy-first",
        ),
    );
    publish_checked(
        &store,
        &proposal(&second, &issue, COMMIT_B, "propose-legacy-second"),
    );
    publish_checked(
        &store,
        &evidence(
            &second,
            ancestry_payload(&second, COMMIT_B, Vec::new(), Vec::new()),
            "ancestry-legacy-second",
        ),
    );

    let landability = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&first, Some("lander"));
    let detail = &landability
        .reasons
        .iter()
        .find(|reason| reason.code == "git_evidence_stale")
        .unwrap()
        .detail;
    assert!(
        detail.contains("legacy receipt without producer snapshot provenance"),
        "{detail}"
    );
    assert!(
        detail.contains("cannot distinguish incomplete input from malformed coverage"),
        "{detail}"
    );
}

#[test]
fn conflicting_determinate_pair_observations_fail_closed() {
    let (_temp, store, issue) = setup();
    let first = ids::new_candidate_id();
    let second = ids::new_candidate_id();
    let first_proposal = publish_checked(
        &store,
        &proposal(&first, &issue, COMMIT_A, "propose-conflict-first"),
    );
    let second_proposal = publish_checked(
        &store,
        &proposal(&second, &issue, COMMIT_B, "propose-conflict-second"),
    );
    publish_checked(
        &store,
        &evidence(
            &first,
            v2_ancestry_payload(
                COMMIT_A,
                vec![(second.clone(), second_proposal.clone())],
                vec![v2_relation(
                    &second,
                    &second_proposal,
                    COMMIT_B,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                )],
                "snapshot-conflict-first",
            ),
            "ancestry-conflict-first",
        ),
    );
    publish_checked(
        &store,
        &evidence(
            &second,
            v2_ancestry_payload(
                COMMIT_B,
                vec![(first.clone(), first_proposal.clone())],
                vec![v2_relation(
                    &first,
                    &first_proposal,
                    COMMIT_A,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::Ancestor,
                    GitRelationKind::Ancestor,
                )],
                "snapshot-conflict-second",
            ),
            "ancestry-conflict-second",
        ),
    );

    let landability = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&first, Some("lander"));
    assert!(
        landability
            .reason_codes
            .contains(&"ancestor_ambiguous".into()),
        "{landability:?}"
    );
    assert!(landability.reasons.iter().any(|reason| {
        reason.code == "ancestor_ambiguous" && reason.detail.contains("conflicting determinate")
    }));
}

#[test]
fn later_snapshot_omission_does_not_erase_an_explicit_pair_fact() {
    let (_temp, store, issue) = setup();
    let first = ids::new_candidate_id();
    let second = ids::new_candidate_id();
    publish_checked(
        &store,
        &proposal(&first, &issue, COMMIT_A, "propose-persistent-first"),
    );
    let second_proposal = publish_checked(
        &store,
        &proposal(&second, &issue, COMMIT_B, "propose-persistent-second"),
    );
    publish_checked(
        &store,
        &evidence(
            &first,
            v2_ancestry_payload(
                COMMIT_A,
                vec![(second.clone(), second_proposal.clone())],
                vec![v2_relation(
                    &second,
                    &second_proposal,
                    COMMIT_B,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                    GitRelationKind::NotAncestor,
                )],
                "snapshot-persistent-pair",
            ),
            "ancestry-persistent-pair",
        ),
    );
    publish_checked(&store, &approve(&first, "review-persistent-first"));
    publish_checked(&store, &authorize(&first, "authorize-persistent-first"));
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .candidate_landability(&first, Some("lander"))
            .landable
    );

    publish_checked(
        &store,
        &evidence(
            &first,
            v2_ancestry_payload(COMMIT_A, Vec::new(), Vec::new(), "snapshot-later-omission"),
            "ancestry-later-omission",
        ),
    );
    let landability = reducer::replay_store(&store)
        .unwrap()
        .candidate_landability(&first, Some("lander"));
    assert!(landability.landable, "{landability:?}");
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
                        base_oid: None,
                        base_relation,
                        relation,
                        subject_to_known_base: None,
                        subject_to_known_tip: None,
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
        role: None,
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
        role: None,
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
fn candidate_cli_records_an_idempotent_git_only_ancestry_override() {
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
                title: Some("CLI ancestry override".into()),
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
            "cli-override-propose",
        ],
    );
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap().to_string();
    let phase_op_id = proposed["phase"]["op_id"].as_str().unwrap().to_string();

    let unauthorized = run_mote(
        temp.path(),
        &[
            "--actor",
            "operator",
            "candidate",
            "evidence",
            "refresh",
            &candidate_id,
            "--idempotency-key",
            "cli-override-unauthorized",
        ],
    );
    assert!(!unauthorized.status.success());
    assert!(
        String::from_utf8_lossy(&unauthorized.stderr).contains("is not a named producer"),
        "{}",
        String::from_utf8_lossy(&unauthorized.stderr)
    );

    let named_producer_override = run_mote(
        temp.path(),
        &[
            "--actor",
            "proposer",
            "candidate",
            "evidence",
            "refresh",
            &candidate_id,
            "--operator-override",
            "--expect-phase",
            &phase_op_id,
            "--reason",
            "operator unavailable",
            "--authority-ref",
            "board:coordination/post-cli",
            "--idempotency-key",
            "cli-override-named-producer",
        ],
    );
    assert!(!named_producer_override.status.success());
    assert!(
        String::from_utf8_lossy(&named_producer_override.stderr)
            .contains("must use ordinary refresh"),
        "{}",
        String::from_utf8_lossy(&named_producer_override.stderr)
    );

    let unauthorized_override = run_mote(
        temp.path(),
        &[
            "--actor",
            "operator",
            "candidate",
            "evidence",
            "refresh",
            &candidate_id,
            "--operator-override",
            "--expect-phase",
            &phase_op_id,
            "--reason",
            "immutable producer is unavailable",
            "--authority-ref",
            "board:coordination/post-cli",
            "--idempotency-key",
            "cli-override-wrong-authority",
        ],
    );
    assert!(!unauthorized_override.status.success());
    assert!(
        String::from_utf8_lossy(&unauthorized_override.stderr)
            .contains("only the immutable proposal authorizer"),
        "{}",
        String::from_utf8_lossy(&unauthorized_override.stderr)
    );

    let args = [
        "--json",
        "--actor",
        "authorizer",
        "candidate",
        "evidence",
        "refresh",
        &candidate_id,
        "--operator-override",
        "--expect-phase",
        &phase_op_id,
        "--reason",
        "immutable producer is unavailable",
        "--authority-ref",
        "board:coordination/post-cli",
        "--idempotency-key",
        "cli-override-valid",
    ];
    let refreshed = run_mote(temp.path(), &args);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let refreshed: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    let override_evidence = refreshed["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evidence| evidence["payload"]["kind"] == "git_ancestry_override")
        .unwrap();
    assert_eq!(
        override_evidence["payload"]["override_basis"]["expect_phase_op_id"],
        phase_op_id
    );
    assert_eq!(
        override_evidence["payload"]["override_basis"]["expect_producers"][0],
        "proposer"
    );
    assert_eq!(override_evidence["refs"][0], "board:coordination/post-cli");

    let op_count = store.list_op_filenames().unwrap().len();
    let retry = run_mote(temp.path(), &args);
    assert!(retry.status.success());
    assert_eq!(store.list_op_filenames().unwrap().len(), op_count);

    abandon(&store, &candidate_id, "cli-override-terminal-transition");
    let terminal_op_count = store.list_op_filenames().unwrap().len();
    let terminal_retry = run_mote(temp.path(), &args);
    assert!(terminal_retry.status.success());
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        terminal_op_count,
        "an exact retry must return the accepted override even after a later phase transition"
    );
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
        repository_id: identity.repository_id.clone(),
        landing_repository_id: identity.repository_id,
        object_format: identity.object_format,
        commit_oid: known_oid.clone(),
        base_oid: root.clone(),
    }];

    let already =
        mote::candidate::probe_ancestry(temp.path(), &descendant, &known_oid, &known).unwrap();
    assert_eq!(already.relation_schema, GIT_RELATION_SCHEMA_V2);
    assert_eq!(
        already.candidate_relations[0].base_oid.as_deref(),
        Some(root.as_str())
    );
    assert_eq!(
        already.candidate_relations[0].base_relation,
        Some(GitRelationKind::Ancestor)
    );
    assert_eq!(
        already.candidate_relations[0].relation,
        GitRelationKind::Ancestor
    );
    assert_eq!(
        already.candidate_relations[0].subject_to_known_base,
        Some(GitRelationKind::NotAncestor)
    );
    assert_eq!(
        already.candidate_relations[0].subject_to_known_tip,
        Some(GitRelationKind::NotAncestor)
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
    run_git(temp.path(), &["branch", "landing-target", &base]);
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
    let reasons = proposed["landability"]["reasons"].as_array().unwrap();
    assert!(!reasons.is_empty());
    assert!(reasons.iter().all(|reason| reason["class"].is_string()));
    assert!(reasons.iter().all(|reason| reason["blocking"] == true));
    assert!(
        reasons
            .iter()
            .any(|reason| { reason["code"] == "review_missing" && reason["class"] == "process" })
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason["code"] == "target_scope_evidence_missing")
    );
    let human = run_mote(temp.path(), &["candidate", "show", candidate_id]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("BLOCKED — process:"));
    assert!(human.contains("review_missing [reviewer]: required reviewer has not approved"));
    assert_eq!(proposed["evidence"][0]["payload"]["relation_schema"], 2);
    let snapshot = &proposed["evidence"][0]["payload"]["producer_snapshot"];
    assert_eq!(snapshot["store_id"], store.read_format().unwrap().store_id);
    assert_eq!(snapshot["replayed_op_count"], 1);
    assert_eq!(
        snapshot["replayed_op_ids_digest"].as_str().unwrap().len(),
        64
    );
    assert!(
        snapshot["observed_candidates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(snapshot["store_git_head"].as_str().unwrap().len(), 40);
    assert!(snapshot["uncommitted_op_count"].as_u64().unwrap() >= 1);
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();

    let refresh_args = [
        "--json",
        "--actor",
        "proposer",
        "candidate",
        "evidence",
        "refresh",
        candidate_id,
        "--idempotency-key",
        "cli-refresh-retry",
    ];
    let refreshed = run_mote(temp.path(), &refresh_args);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let op_count_after_refresh = store.list_op_filenames().unwrap().len();
    let retried = run_mote(temp.path(), &refresh_args);
    assert!(
        retried.status.success(),
        "{}",
        String::from_utf8_lossy(&retried.stderr)
    );
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_after_refresh,
        "an idempotent refresh retry must not publish a snapshot with a changed digest"
    );
    let target_scope = run_mote(
        temp.path(),
        &[
            "--json",
            "--actor",
            "proposer",
            "candidate",
            "evidence",
            "target-scope",
            candidate_id,
            "--target",
            "landing-target",
            "--idempotency-key",
            "cli-target-scope",
        ],
    );
    assert!(
        target_scope.status.success(),
        "{}",
        String::from_utf8_lossy(&target_scope.stderr)
    );

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

    run_git(temp.path(), &["branch", "-f", "landing-target", "HEAD"]);

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
            "landing-target",
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
fn candidate_cli_reconciles_out_of_band_without_rewriting_governance() {
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
    let candidate_oid = run_git(temp.path(), &["rev-parse", "HEAD"]);

    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("out-of-band candidate".into()),
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
            "reconcile-cli-propose",
        ],
    );
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap().to_string();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap().to_string();

    let reserved = run_mote(
        temp.path(),
        &[
            "--actor",
            "proposer",
            "reserve",
            "work.txt",
            "--candidate",
            &candidate_id,
        ],
    );
    assert!(reserved.status.success());

    let op_count_before_intruder = store.list_op_filenames().unwrap().len();
    let intruder = run_mote(
        temp.path(),
        &[
            "--actor",
            "intruder",
            "candidate",
            "reconcile",
            &candidate_id,
            "--target",
            "HEAD",
            "--expect-phase",
            &phase_op,
            "--idempotency-key",
            "reconcile-cli-intruder",
        ],
    );
    assert_eq!(intruder.status.code(), Some(2));
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_before_intruder,
        "authority is checked before probing or publishing evidence"
    );

    let reconcile_args = [
        "--json",
        "--actor",
        "authorizer",
        "candidate",
        "reconcile",
        &candidate_id,
        "--target",
        "HEAD",
        "--expect-phase",
        &phase_op,
        "--idempotency-key",
        "reconcile-cli-success",
    ];
    let reconciled = run_mote(temp.path(), &reconcile_args);
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let reconciled: serde_json::Value = serde_json::from_slice(&reconciled.stdout).unwrap();
    assert_eq!(reconciled["phase"]["value"], "landed_out_of_band");
    assert!(reconciled["landing"].is_null());
    assert!(reconciled["authorization"].is_null());
    assert_eq!(
        reconciled["reconciliation"]["authority"],
        "proposal_authorizer"
    );
    assert_eq!(reconciled["reconciliation"]["target_oid"], candidate_oid);
    assert_eq!(
        reconciled["reconciliation"]["policy_snapshot"]["authorization_op_id"],
        serde_json::Value::Null
    );
    let preserved_codes = reconciled["reconciliation"]["policy_snapshot"]
        ["pre_transition_landability"]["reason_codes"]
        .as_array()
        .unwrap();
    assert!(preserved_codes.iter().any(|code| code == "review_missing"));
    assert!(
        preserved_codes
            .iter()
            .any(|code| code == "authorization_absent")
    );
    assert_eq!(reconciled["reservations"][0]["disposition"], "orphaned");
    assert!(
        reconciled["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| {
                receipt["name"] == GIT_REACHABILITY_EVIDENCE
                    && receipt["outcome"] == "pass"
                    && receipt["payload"]["kind"] == "git_reachability"
            })
    );

    let op_count_after_success = store.list_op_filenames().unwrap().len();
    let retry = run_mote(temp.path(), &reconcile_args);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_after_success,
        "an exact reconciliation retry must not re-probe or publish"
    );

    let human = run_mote(temp.path(), &["candidate", "show", &candidate_id]);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("landed_out_of_band"));
    assert!(human.contains("formal review/authorization did not govern this landing"));
    assert!(human.contains("preserved pre-transition blockers"));

    let listed = run_mote(
        temp.path(),
        &[
            "--json",
            "candidate",
            "list",
            "--phase",
            "landed_out_of_band",
        ],
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let events = run_mote(temp.path(), &["--json", "events", "--kind", "candidate"]);
    let events: Vec<serde_json::Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event = events
        .iter()
        .find(|event| event["type"] == "candidate.landed_out_of_band")
        .unwrap();
    assert_eq!(event["data"]["candidate_id"], candidate_id);
    assert_eq!(
        event["data"]["lease_effects"]["orphaned_reservations"][0]["disposition"],
        "orphaned"
    );

    let audit = run_mote(temp.path(), &["--json", "audit", "--fail-on", "never"]);
    assert!(
        audit.status.success(),
        "{}",
        String::from_utf8_lossy(&audit.stderr)
    );
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    let finding = audit["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "candidate_out_of_band_landing_recorded")
        .unwrap();
    assert_eq!(finding["severity"], "warning");
    assert_eq!(finding["evidence"]["target_oid"], candidate_oid);
    assert!(
        finding["message"]
            .as_str()
            .unwrap()
            .contains("outside the formal candidate transition")
    );
}

#[test]
fn candidate_cli_records_an_explicit_operator_override_without_claiming_authorizer_identity() {
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
    let candidate_oid = run_git(temp.path(), &["rev-parse", "HEAD"]);

    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("operator recovery candidate".into()),
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
            "departed-authorizer",
            "--reviewer",
            "reviewer",
            "--idempotency-key",
            "operator-override-cli-propose",
        ],
    );
    assert!(proposed.status.success());
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap().to_string();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap().to_string();

    let reconcile_args = [
        "--json",
        "--actor",
        "recovery-operator",
        "candidate",
        "reconcile",
        &candidate_id,
        "--target",
        "HEAD",
        "--expect-phase",
        &phase_op,
        "--operator-override",
        "--reason",
        "owner appointed a successor coordinator",
        "--authority-ref",
        "post-appointment-record",
        "--authority-ref",
        "post-independent-ack",
        "--idempotency-key",
        "operator-override-cli-success",
    ];
    let reconciled = run_mote(temp.path(), &reconcile_args);
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let reconciled: serde_json::Value = serde_json::from_slice(&reconciled.stdout).unwrap();
    assert_eq!(reconciled["phase"]["value"], "landed_out_of_band");
    assert_eq!(reconciled["policy"]["authorizer"], "departed-authorizer");
    assert_eq!(
        reconciled["reconciliation"]["authority"],
        "explicit_operator_override"
    );
    assert_eq!(
        reconciled["reconciliation"]["override_basis"]["expect_authorizer"],
        "departed-authorizer"
    );
    assert_eq!(
        reconciled["reconciliation"]["override_basis"]["reason"],
        "owner appointed a successor coordinator"
    );
    assert_eq!(
        reconciled["reconciliation"]["override_basis"]["authority_refs"],
        serde_json::json!(["post-appointment-record", "post-independent-ack"])
    );
    assert_eq!(reconciled["reconciliation"]["target_oid"], candidate_oid);

    let op_count_after_success = store.list_op_filenames().unwrap().len();
    let retry = run_mote(temp.path(), &reconcile_args);
    assert!(retry.status.success());
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_after_success,
        "an exact operator-override retry must not re-probe or publish"
    );

    let human = run_mote(temp.path(), &["candidate", "show", &candidate_id]);
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("authority=explicit_operator_override"));
    assert!(human.contains("expected-authorizer=departed-authorizer"));
    assert!(human.contains("owner appointed a successor coordinator"));

    let events = run_mote(temp.path(), &["--json", "events", "--kind", "candidate"]);
    let event = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["type"] == "candidate.landed_out_of_band")
        .unwrap();
    assert_eq!(
        event["data"]["override_basis"]["expect_authorizer"],
        "departed-authorizer"
    );

    let audit = run_mote(temp.path(), &["--json", "audit", "--fail-on", "never"]);
    assert!(audit.status.success());
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    let finding = audit["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "candidate_reconciliation_operator_override_recorded")
        .unwrap();
    assert_eq!(finding["severity"], "warning");
    assert_eq!(
        finding["evidence"]["expected_authorizer"],
        "departed-authorizer"
    );
}

#[test]
fn candidate_cli_recovers_a_legacy_abandonment_without_rewriting_it() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(temp.path(), &["config", "user.email", "test@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Test"]);
    std::fs::write(temp.path().join("work.txt"), "base\n").unwrap();
    run_git(temp.path(), &["add", "work.txt"]);
    run_git(temp.path(), &["commit", "-qm", "base"]);
    let base = run_git(temp.path(), &["rev-parse", "HEAD"]);
    run_git(temp.path(), &["branch", "not-landed", &base]);
    std::fs::write(temp.path().join("work.txt"), "candidate\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "candidate"]);
    let candidate_oid = run_git(temp.path(), &["rev-parse", "HEAD"]);

    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("legacy abandoned candidate".into()),
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
            "departed-authorizer",
            "--reviewer",
            "reviewer",
            "--idempotency-key",
            "legacy-abandoned-cli-propose",
        ],
    );
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap().to_string();
    let proposal_phase = proposed["phase"]["op_id"].as_str().unwrap().to_string();
    let abandonment_op = publish_checked(
        &store,
        &Op::CandidateAbandon(CandidateAbandonOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "departed-authorizer".into(),
            candidate_id: candidate_id.clone(),
            expect_phase: proposal_phase.clone(),
            reason: Some("commit was already present on main".into()),
            idempotency_key: "legacy-abandoned-cli-abandon".into(),
        }),
    );

    let op_count_before_refusals = store.list_op_filenames().unwrap().len();
    let ordinary = run_mote(
        temp.path(),
        &[
            "--actor",
            "departed-authorizer",
            "candidate",
            "reconcile",
            &candidate_id,
            "--target",
            "HEAD",
            "--expect-phase",
            &abandonment_op,
            "--idempotency-key",
            "legacy-abandoned-cli-ordinary-refusal",
        ],
    );
    assert_eq!(ordinary.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&ordinary.stderr).contains("explicit operator override"),
        "{}",
        String::from_utf8_lossy(&ordinary.stderr)
    );

    let stale_phase = run_mote(
        temp.path(),
        &[
            "--actor",
            "recovery-operator",
            "candidate",
            "reconcile",
            &candidate_id,
            "--target",
            "HEAD",
            "--expect-phase",
            &proposal_phase,
            "--operator-override",
            "--reason",
            "legacy abandonment correction",
            "--authority-ref",
            "post-legacy-correction",
            "--idempotency-key",
            "legacy-abandoned-cli-stale-phase",
        ],
    );
    assert_eq!(stale_phase.status.code(), Some(2));
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_before_refusals,
        "authority and phase refusals must occur before publishing Git evidence"
    );

    let failed_reachability = run_mote(
        temp.path(),
        &[
            "--actor",
            "recovery-operator",
            "candidate",
            "reconcile",
            &candidate_id,
            "--target",
            "not-landed",
            "--expect-phase",
            &abandonment_op,
            "--operator-override",
            "--reason",
            "legacy abandonment correction",
            "--authority-ref",
            "post-legacy-correction",
            "--idempotency-key",
            "legacy-abandoned-cli-failed-reachability",
        ],
    );
    assert_eq!(failed_reachability.status.code(), Some(2));
    let state_after_failed_reachability = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state_after_failed_reachability.candidates[&candidate_id]
            .phase
            .as_str(),
        "abandoned"
    );
    assert!(
        state_after_failed_reachability.candidates[&candidate_id]
            .evidence
            .values()
            .any(|receipt| {
                receipt.name == GIT_REACHABILITY_EVIDENCE
                    && receipt.producer == "recovery-operator"
                    && receipt.outcome == EvidenceOutcome::Fail
            })
    );

    let reconcile_args = [
        "--json",
        "--actor",
        "recovery-operator",
        "candidate",
        "reconcile",
        &candidate_id,
        "--target",
        "HEAD",
        "--expect-phase",
        &abandonment_op,
        "--operator-override",
        "--reason",
        "legacy abandonment correction",
        "--authority-ref",
        "post-legacy-correction",
        "--idempotency-key",
        "legacy-abandoned-cli-success",
    ];
    let reconciled = run_mote(temp.path(), &reconcile_args);
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let reconciled: serde_json::Value = serde_json::from_slice(&reconciled.stdout).unwrap();
    assert_eq!(reconciled["phase"]["value"], "landed_out_of_band");
    assert_eq!(reconciled["reconciliation"]["target_oid"], candidate_oid);
    assert_eq!(
        reconciled["reconciliation"]["authority"],
        "explicit_operator_override"
    );

    let op_count_after_success = store.list_op_filenames().unwrap().len();
    let retry = run_mote(temp.path(), &reconcile_args);
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        op_count_after_success,
        "an exact legacy-recovery retry must not re-probe or publish"
    );

    let final_state = reducer::replay_store(&store).unwrap();
    assert!(final_state.was_accepted(&abandonment_op));
    assert!(
        store
            .ops_dir()
            .join(format!("{abandonment_op}.json"))
            .is_file(),
        "the historical abandonment must remain an immutable accepted operation"
    );
}

#[test]
fn candidate_cli_reconciliation_rejects_non_ancestor_target() {
    let temp = TempDir::new().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(temp.path(), &["config", "user.email", "test@example.com"]);
    run_git(temp.path(), &["config", "user.name", "Test"]);
    std::fs::write(temp.path().join("work.txt"), "base\n").unwrap();
    run_git(temp.path(), &["add", "work.txt"]);
    run_git(temp.path(), &["commit", "-qm", "base"]);
    let base = run_git(temp.path(), &["rev-parse", "HEAD"]);
    run_git(temp.path(), &["branch", "target", &base]);
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
                title: Some("non-ancestor candidate".into()),
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
            "non-ancestor-propose",
        ],
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();
    let rejected = run_mote(
        temp.path(),
        &[
            "--actor",
            "authorizer",
            "candidate",
            "reconcile",
            candidate_id,
            "--target",
            "target",
            "--expect-phase",
            phase_op,
            "--idempotency-key",
            "non-ancestor-reconcile",
        ],
    );
    assert_eq!(rejected.status.code(), Some(2));
    let state = reducer::replay_store(&store).unwrap();
    let candidate = &state.candidates[candidate_id];
    assert_eq!(candidate.phase.as_str(), "pending");
    assert!(candidate.reconciled.is_none());
    let receipt = candidate
        .evidence
        .values()
        .find(|receipt| receipt.name == GIT_REACHABILITY_EVIDENCE)
        .unwrap();
    assert_eq!(receipt.outcome, EvidenceOutcome::Fail);
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
    run_git(temp.path(), &["branch", "landing-target", &landed]);

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
    let target_scope = run_mote(
        temp.path(),
        &[
            "--actor",
            "proposer",
            "candidate",
            "evidence",
            "target-scope",
            &new_id,
            "--target",
            "landing-target",
            "--idempotency-key",
            "new-target-scope",
        ],
    );
    assert!(
        target_scope.status.success(),
        "{}",
        String::from_utf8_lossy(&target_scope.stderr)
    );
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
