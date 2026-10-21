use jiff::Timestamp;
use mote::{
    candidate::{
        CandidateEvidencePayload, EvidenceOutcome, EvidenceRequirement, GIT_ANCESTRY_EVIDENCE,
        GitAncestryReceipt, evidence_id,
    },
    canonical,
    op::{CandidateEvidenceOp, CandidateProposeOp, Op, ScalarSet, make_create},
    reducer,
    state::State,
};

fn canonical_state_bytes(state: &State) -> Vec<u8> {
    canonical::encode(&serde_json::to_value(state).unwrap())
}

fn state_with_candidate_evidence() -> State {
    let candidate_oid = "a".repeat(40);
    let base_oid = "b".repeat(40);
    let mut bead = make_create(
        "alice".into(),
        "bd-evidence".into(),
        ScalarSet {
            title: Some("candidate evidence fixture".into()),
            ..Default::default()
        },
        Timestamp::now(),
    );
    let Op::Create(create) = &mut bead else {
        unreachable!();
    };
    create.op = "bead-create".into();
    let proposal = Op::CandidatePropose(CandidateProposeOp {
        v: 1,
        op: "candidate-proposal".into(),
        ts: "2026-01-01T00:00:00Z".into(),
        actor: "proposer".into(),
        candidate_id: "cand-01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        entity: "bd-evidence".into(),
        store_id: "st-evidence".into(),
        repository_id: "repo-evidence".into(),
        landing_repository_id: None,
        object_source: None,
        object_format: "sha1".into(),
        commit_oid: candidate_oid.clone(),
        base_oid: base_oid.clone(),
        parent_oids: vec![base_oid.clone()],
        paths: vec!["src/state.rs".into()],
        authorizer: "authorizer".into(),
        reviewers: vec!["reviewer".into()],
        review_policy: None,
        evidence_requirements: vec![EvidenceRequirement {
            name: GIT_ANCESTRY_EVIDENCE.into(),
            kind: "git".into(),
            producers: vec!["proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: "proposal-key".into(),
    });
    let payload = CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        relation_schema: 1,
        repository_id: "repo-evidence".into(),
        object_format: "sha1".into(),
        common_dir_hash: "common-dir".into(),
        commit_oid: candidate_oid.clone(),
        base_oid: base_oid.clone(),
        parent_oids: vec![base_oid],
        base_is_ancestor: Some(true),
        candidate_relations: Vec::new(),
        covered_candidates: Vec::new(),
        producer_snapshot: None,
        git_version: "git test".into(),
        detail: None,
    });
    let evidence = Op::CandidateEvidence(CandidateEvidenceOp {
        v: 1,
        op: "candidate-evidence".into(),
        ts: "2026-01-01T00:00:01Z".into(),
        actor: "proposer".into(),
        candidate_id: "cand-01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        candidate_oid,
        evidence_id: evidence_id(&payload).unwrap(),
        name: GIT_ANCESTRY_EVIDENCE.into(),
        evidence_kind: "git".into(),
        producer_tool: "git test".into(),
        outcome: EvidenceOutcome::Pass,
        payload,
        refs: Vec::new(),
        idempotency_key: "evidence-key".into(),
    });

    reducer::replay([
        (
            "bead-create.json".into(),
            serde_json::to_vec(&bead).unwrap(),
        ),
        (
            "candidate-proposal.json".into(),
            serde_json::to_vec(&proposal).unwrap(),
        ),
        (
            "candidate-evidence.json".into(),
            serde_json::to_vec(&evidence).unwrap(),
        ),
    ])
}

#[test]
fn populated_tuple_key_registry_serializes_as_ordered_key_value_pairs() {
    let mut state = State::default();
    state.handoff_idempotency.insert(
        ("zoe".into(), "retry-2".into()),
        ("accepted".into(), "op-2".into()),
    );
    state.handoff_idempotency.insert(
        ("alice".into(), "retry-1".into()),
        ("conflict".into(), "op-1".into()),
    );

    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(
        value["handoff_idempotency"],
        serde_json::json!([
            [["alice", "retry-1"], ["conflict", "op-1"]],
            [["zoe", "retry-2"], ["accepted", "op-2"]],
        ])
    );
}

#[test]
fn equivalent_replays_have_identical_canonical_state_bytes() {
    let op = make_create(
        "alice".into(),
        "bd-state-evidence".into(),
        ScalarSet {
            title: Some("preserved reducer state".into()),
            ..Default::default()
        },
        Timestamp::now(),
    );
    let entry = (
        format!("{}.json", op.op_id()),
        serde_json::to_vec(&op).unwrap(),
    );

    let first = reducer::replay([entry.clone()]);
    let second = reducer::replay([entry]);

    assert_eq!(
        canonical_state_bytes(&first),
        canonical_state_bytes(&second)
    );
}

#[test]
fn non_task_state_change_changes_canonical_bytes_and_digest() {
    let before = State::default();
    let mut after = State::default();
    after
        .board_read_cursors
        .insert("alice".into(), "op-read-cursor".into());

    let before_bytes = canonical_state_bytes(&before);
    let after_bytes = canonical_state_bytes(&after);
    assert_ne!(before_bytes, after_bytes);
    assert_ne!(blake3::hash(&before_bytes), blake3::hash(&after_bytes));
}

#[test]
fn rejected_orphan_history_changes_canonical_bytes_and_digest() {
    let first = reducer::replay([("orphan.json".into(), b"not-json".to_vec())]);
    let second = reducer::replay([("orphan.json".into(), b"also-not-json".to_vec())]);

    assert_eq!(first.orphan_history.len(), 1);
    assert_eq!(second.orphan_history.len(), 1);

    let first_bytes = canonical_state_bytes(&first);
    let second_bytes = canonical_state_bytes(&second);
    assert_ne!(first_bytes, second_bytes);
    assert_ne!(blake3::hash(&first_bytes), blake3::hash(&second_bytes));
}

#[test]
fn state_candidate_evidence_preserves_tuple_keys_without_changing_public_candidate_json() {
    let first = state_with_candidate_evidence();
    let mut second = first.clone();
    let evidence = second
        .candidates
        .get_mut("cand-01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .unwrap()
        .evidence
        .remove(&(GIT_ANCESTRY_EVIDENCE.into(), "proposer".into()))
        .unwrap();
    second
        .candidates
        .get_mut("cand-01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .unwrap()
        .evidence
        .insert(("other-evidence".into(), "other-producer".into()), evidence);
    let candidate = &first.candidates["cand-01ARZ3NDEKTSV4RRFFQ69G5FAV"];

    let standalone = serde_json::to_value(candidate).unwrap();
    assert!(standalone["evidence"].is_array());
    assert_eq!(standalone["evidence"][0]["producer"], "proposer");

    let state_value = serde_json::to_value(&first).unwrap();
    let state_evidence = &state_value["candidates"]["cand-01ARZ3NDEKTSV4RRFFQ69G5FAV"]["evidence"];
    assert_eq!(
        state_evidence[0][0],
        serde_json::json!([GIT_ANCESTRY_EVIDENCE, "proposer"])
    );
    assert_eq!(state_evidence[0][1]["producer"], "proposer");

    let first_bytes = canonical_state_bytes(&first);
    let second_bytes = canonical_state_bytes(&second);
    assert_ne!(first_bytes, second_bytes);
    assert_ne!(blake3::hash(&first_bytes), blake3::hash(&second_bytes));
}
