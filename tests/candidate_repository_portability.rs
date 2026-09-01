use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use jiff::Timestamp;
use tempfile::TempDir;

use mote::candidate::{
    AuthorizationStatus, CANDIDATE_PORTABLE_PROTOCOL_VERSION, CANDIDATE_PROTOCOL_VERSION,
    CANDIDATE_ROLE_REVIEW_VERSION, CandidateEvidencePayload, CandidateObjectSource,
    CandidateReviewPolicy, EvidenceOutcome, EvidenceRequirement, GIT_ANCESTRY_EVIDENCE,
    GIT_OBJECT_AVAILABILITY_EVIDENCE, GIT_RECONCILIATION_REACHABILITY_EVIDENCE, GitAncestryReceipt,
    GitObjectAvailabilityReceipt, ReviewVerdict,
};
use mote::ids;
use mote::op::{
    CandidateAuthorizeOp, CandidateEvidenceOp, CandidateLandingRepositoryBindOp,
    CandidateProposeOp, CandidateReviewOp, Op, ScalarSet, make_create,
};
use mote::{publish, reducer, repo::Store};

fn run_git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn run_mote(cwd: &Path, store_parent: &Path, actor: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(cwd)
        .arg("--store")
        .arg(store_parent)
        .arg("--actor")
        .arg(actor)
        .args(args)
        .output()
        .unwrap()
}

struct Repositories {
    _temp: TempDir,
    canonical: PathBuf,
    source: PathBuf,
    store: Store,
    issue: String,
    base: String,
}

fn repositories() -> Repositories {
    let temp = TempDir::new().unwrap();
    let canonical = temp.path().join("canonical");
    std::fs::create_dir(&canonical).unwrap();
    run_git(&canonical, &["init", "-q", "-b", "main"]);
    run_git(&canonical, &["config", "user.email", "test@example.com"]);
    run_git(&canonical, &["config", "user.name", "Test"]);
    std::fs::write(canonical.join("work.txt"), "base\n").unwrap();
    run_git(&canonical, &["add", "work.txt"]);
    run_git(&canonical, &["commit", "-qm", "base"]);
    let base = run_git(&canonical, &["rev-parse", "HEAD"]);
    let store = Store::init(&canonical).unwrap();
    let issue = ids::new_bead_id();
    publish_checked(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("portable candidate".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    );
    let source = temp.path().join("source");
    let clone = Command::new("git")
        .arg("clone")
        .arg("-q")
        .arg(&canonical)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
    run_git(&source, &["config", "user.email", "test@example.com"]);
    run_git(&source, &["config", "user.name", "Test"]);
    Repositories {
        _temp: temp,
        canonical,
        source,
        store,
        issue,
        base,
    }
}

fn source_commit(repositories: &Repositories, body: &str) -> String {
    std::fs::write(repositories.source.join("work.txt"), format!("{body}\n")).unwrap();
    run_git(&repositories.source, &["commit", "-qam", body]);
    run_git(&repositories.source, &["rev-parse", "HEAD"])
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

fn review(candidate_id: &str, key: &str) -> Op {
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

#[test]
fn standalone_proposal_blocks_until_shared_repository_has_object_then_lands() {
    let repositories = repositories();
    let commit = source_commit(&repositories, "candidate");
    let source_locator = repositories.source.to_str().unwrap();
    let proposed = run_mote(
        &repositories.source,
        &repositories.canonical,
        "proposer",
        &[
            "--json",
            "candidate",
            "propose",
            "--issue",
            &repositories.issue,
            "--commit",
            &commit,
            "--base",
            &repositories.base,
            "--path",
            "work.txt",
            "--authorizer",
            "authorizer",
            "--reviewer",
            "reviewer",
            "--object-source",
            source_locator,
            "--idempotency-key",
            "portable-proposal",
        ],
    );
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&proposed.stderr)
            .contains("is not readable from the shared landing repository")
    );
    let proposed: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();
    assert_eq!(
        proposed["policy"]["review_policy_version"],
        CANDIDATE_PORTABLE_PROTOCOL_VERSION
    );
    assert_ne!(
        proposed["identity"]["proposal_repository_id"],
        proposed["identity"]["landing_repository_id"]
    );
    assert_eq!(
        proposed["identity"]["object_source"]["locator"],
        source_locator
    );
    assert!(
        proposed["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|code| code == "object_unreachable")
    );

    run_git(&repositories.canonical, &["fetch", source_locator, "main"]);
    let available = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "authorizer",
        &[
            "--json",
            "candidate",
            "evidence",
            "availability",
            candidate_id,
            "--idempotency-key",
            "availability-after-transfer",
        ],
    );
    assert!(
        available.status.success(),
        "{}",
        String::from_utf8_lossy(&available.stderr)
    );
    let available: serde_json::Value = serde_json::from_slice(&available.stdout).unwrap();
    assert!(
        !available["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|code| code == "object_unreachable")
    );
    let target_scope = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "authorizer",
        &[
            "--json",
            "candidate",
            "evidence",
            "target-scope",
            candidate_id,
            "--target",
            "HEAD",
            "--idempotency-key",
            "portable-target-scope",
        ],
    );
    assert!(
        target_scope.status.success(),
        "{}",
        String::from_utf8_lossy(&target_scope.stderr)
    );

    let reviewed = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "reviewer",
        &[
            "--json",
            "candidate",
            "review",
            candidate_id,
            "approve",
            "--idempotency-key",
            "portable-review",
        ],
    );
    assert!(
        reviewed.status.success(),
        "{}",
        String::from_utf8_lossy(&reviewed.stderr)
    );
    let authorized = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "authorizer",
        &[
            "--json",
            "candidate",
            "authorize",
            candidate_id,
            "--grantee",
            "lander",
            "--idempotency-key",
            "portable-authorization",
        ],
    );
    assert!(
        authorized.status.success(),
        "{}",
        String::from_utf8_lossy(&authorized.stderr)
    );
    let authorized: serde_json::Value = serde_json::from_slice(&authorized.stdout).unwrap();
    assert_eq!(authorized["landability"]["landable"], true);
    let authorization_op = authorized["authorization"]["op_id"].as_str().unwrap();

    run_git(
        &repositories.canonical,
        &["merge", "--ff-only", "FETCH_HEAD"],
    );
    let landed = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "lander",
        &[
            "--json",
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
            "portable-landing",
        ],
    );
    assert!(
        landed.status.success(),
        "{}",
        String::from_utf8_lossy(&landed.stderr)
    );
    let landed: serde_json::Value = serde_json::from_slice(&landed.stdout).unwrap();
    assert_eq!(landed["phase"]["value"], "landed");
}

#[test]
fn legacy_source_bound_candidate_can_be_explicitly_rebound_and_terminalized() {
    let repositories = repositories();
    let commit = source_commit(&repositories, "legacy candidate");
    let source_locator = repositories.source.to_str().unwrap();
    run_git(&repositories.canonical, &["fetch", source_locator, "main"]);

    let ancestry =
        mote::candidate::probe_ancestry(&repositories.source, &commit, &repositories.base, &[])
            .unwrap();
    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &repositories.store,
        &Op::CandidatePropose(CandidateProposeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            entity: repositories.issue.clone(),
            store_id: repositories.store.read_format().unwrap().store_id,
            repository_id: ancestry.repository_id.clone(),
            landing_repository_id: None,
            object_source: None,
            object_format: ancestry.object_format.clone(),
            commit_oid: commit.clone(),
            base_oid: repositories.base.clone(),
            parent_oids: ancestry.parent_oids.clone(),
            paths: vec!["work.txt".into()],
            authorizer: "authorizer".into(),
            reviewers: vec!["reviewer".into()],
            review_policy: None,
            evidence_requirements: vec![EvidenceRequirement {
                name: GIT_ANCESTRY_EVIDENCE.into(),
                kind: "git".into(),
                producers: vec!["proposer".into()],
            }],
            evidence_refs: Vec::new(),
            idempotency_key: "legacy-proposal".into(),
        }),
    );
    let ancestry_payload = CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        producer_snapshot: None,
        ..ancestry
    });
    publish_checked(
        &repositories.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            candidate_oid: commit.clone(),
            evidence_id: mote::candidate::evidence_id(&ancestry_payload).unwrap(),
            name: GIT_ANCESTRY_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload: ancestry_payload,
            refs: Vec::new(),
            idempotency_key: "legacy-ancestry".into(),
        }),
    );
    publish_checked(&repositories.store, &review(&candidate_id, "legacy-review"));
    let authorization_op = publish_checked(
        &repositories.store,
        &authorize(&candidate_id, "legacy-authorize"),
    );

    let before = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "lander",
        &[
            "candidate",
            "landed",
            &candidate_id,
            "--target",
            "HEAD",
            "--expect-phase",
            &proposal_op,
            "--expect-authorization",
            &authorization_op,
            "--idempotency-key",
            "legacy-landing-before-bind",
        ],
    );
    assert!(!before.status.success());
    assert!(
        String::from_utf8_lossy(&before.stderr)
            .contains("landing repository identity does not match candidate")
    );

    let rebound = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "authorizer",
        &[
            "--json",
            "candidate",
            "bind-landing-repository",
            &candidate_id,
            "--expect-phase",
            &proposal_op,
            "--expect-repository",
            &proposal_op,
            "--object-source",
            source_locator,
            "--reason",
            "bind legacy standalone proposal to shared repository",
            "--idempotency-key",
            "legacy-bind",
        ],
    );
    assert!(
        rebound.status.success(),
        "{}",
        String::from_utf8_lossy(&rebound.stderr)
    );
    let rebound: serde_json::Value = serde_json::from_slice(&rebound.stdout).unwrap();
    assert_ne!(
        rebound["identity"]["proposal_repository_id"],
        rebound["identity"]["landing_repository_id"]
    );
    assert_eq!(
        rebound["identity"]["landing_repository_bindings"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        rebound["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|code| code == "target_scope_evidence_missing")
    );
    let target_scope = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "authorizer",
        &[
            "--json",
            "candidate",
            "evidence",
            "target-scope",
            &candidate_id,
            "--target",
            "HEAD",
            "--idempotency-key",
            "legacy-target-scope-after-bind",
        ],
    );
    assert!(
        target_scope.status.success(),
        "{}",
        String::from_utf8_lossy(&target_scope.stderr)
    );
    let target_scope: serde_json::Value = serde_json::from_slice(&target_scope.stdout).unwrap();
    assert_eq!(target_scope["landability"]["landable"], true);

    run_git(
        &repositories.canonical,
        &["merge", "--ff-only", "FETCH_HEAD"],
    );

    let landed = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "lander",
        &[
            "--json",
            "candidate",
            "landed",
            &candidate_id,
            "--target",
            "HEAD",
            "--expect-phase",
            &proposal_op,
            "--expect-authorization",
            &authorization_op,
            "--idempotency-key",
            "legacy-landing-after-bind",
        ],
    );
    assert!(
        landed.status.success(),
        "{}",
        String::from_utf8_lossy(&landed.stderr)
    );
    let landed: serde_json::Value = serde_json::from_slice(&landed.stdout).unwrap();
    assert_eq!(landed["phase"]["value"], "landed");
}

#[test]
fn operator_reconciles_across_repositories_without_rewriting_provenance() {
    let repositories = repositories();
    let commit = source_commit(&repositories, "already landed candidate");
    let source_locator = repositories.source.to_str().unwrap();
    run_git(&repositories.canonical, &["fetch", source_locator, "main"]);
    run_git(
        &repositories.canonical,
        &["merge", "--ff-only", "FETCH_HEAD"],
    );

    let ancestry =
        mote::candidate::probe_ancestry(&repositories.source, &commit, &repositories.base, &[])
            .unwrap();
    let canonical_repository_id =
        mote::candidate::probe_ancestry(&repositories.canonical, &commit, &repositories.base, &[])
            .unwrap()
            .repository_id;
    assert_ne!(ancestry.repository_id, canonical_repository_id);

    let candidate_id = ids::new_candidate_id();
    let proposal_op = publish_checked(
        &repositories.store,
        &Op::CandidatePropose(CandidateProposeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            entity: repositories.issue.clone(),
            store_id: repositories.store.read_format().unwrap().store_id,
            repository_id: ancestry.repository_id.clone(),
            landing_repository_id: None,
            object_source: None,
            object_format: ancestry.object_format.clone(),
            commit_oid: commit.clone(),
            base_oid: repositories.base.clone(),
            parent_oids: ancestry.parent_oids.clone(),
            paths: vec!["work.txt".into()],
            authorizer: "departed-authorizer".into(),
            reviewers: vec!["reviewer".into()],
            review_policy: None,
            evidence_requirements: vec![EvidenceRequirement {
                name: GIT_ANCESTRY_EVIDENCE.into(),
                kind: "git".into(),
                producers: vec!["proposer".into()],
            }],
            evidence_refs: Vec::new(),
            idempotency_key: "cross-repository-proposal".into(),
        }),
    );
    let ancestry_payload = CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        producer_snapshot: None,
        ..ancestry.clone()
    });
    publish_checked(
        &repositories.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            candidate_oid: commit.clone(),
            evidence_id: mote::candidate::evidence_id(&ancestry_payload).unwrap(),
            name: GIT_ANCESTRY_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload: ancestry_payload,
            refs: Vec::new(),
            idempotency_key: "cross-repository-ancestry".into(),
        }),
    );
    publish_checked(
        &repositories.store,
        &review(&candidate_id, "cross-repository-review"),
    );

    let reconcile_args = [
        "--json",
        "candidate",
        "reconcile",
        &candidate_id,
        "--target",
        "HEAD",
        "--expect-phase",
        &proposal_op,
        "--operator-override",
        "--reason",
        "owner appointed a successor coordinator",
        "--authority-ref",
        "post-appointment-record",
        "--authority-ref",
        "post-independent-ack",
        "--idempotency-key",
        "cross-repository-reconciliation",
    ];
    let reconciled = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "recovery-operator",
        &reconcile_args,
    );
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let reconciled: serde_json::Value = serde_json::from_slice(&reconciled.stdout).unwrap();
    assert_eq!(reconciled["phase"]["value"], "landed_out_of_band");
    assert_eq!(
        reconciled["identity"]["proposal_repository_id"],
        ancestry.repository_id
    );
    assert_eq!(
        reconciled["identity"]["landing_repository_id"],
        ancestry.repository_id
    );
    assert_eq!(
        reconciled["identity"]["landing_repository_bindings"],
        serde_json::json!([])
    );
    assert_eq!(reconciled["policy"]["authorizer"], "departed-authorizer");
    assert_eq!(
        reconciled["reconciliation"]["override_basis"]["expect_landing_repository_id"],
        ancestry.repository_id
    );
    assert_eq!(
        reconciled["reconciliation"]["repository_bridge"]["expect_landing_repository_op_id"],
        proposal_op
    );
    assert_eq!(
        reconciled["reconciliation"]["repository_bridge"]["object_availability"]["repository_id"],
        canonical_repository_id
    );
    assert_eq!(
        reconciled["reconciliation"]["repository_bridge"]["object_availability"]["candidate_oid"],
        commit
    );
    assert_eq!(
        reconciled["reconciliation"]["repository_bridge"]["object_availability"]["observed_parent_oids"],
        serde_json::json!([repositories.base])
    );
    assert!(
        reconciled["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| {
                receipt["name"] == GIT_RECONCILIATION_REACHABILITY_EVIDENCE
                    && receipt["outcome"] == "pass"
                    && receipt["payload"]["repository_id"] == canonical_repository_id
            })
    );

    let human = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "recovery-operator",
        &["candidate", "show", &candidate_id],
    );
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("reconciliation repository bridge"));
    assert!(human.contains(&canonical_repository_id));

    let audit = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "recovery-operator",
        &["--json", "audit", "--fail-on", "never"],
    );
    assert!(audit.status.success());
    let audit: serde_json::Value = serde_json::from_slice(&audit.stdout).unwrap();
    let finding = audit["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "candidate_reconciliation_repository_bridge_recorded")
        .unwrap();
    assert_eq!(
        finding["evidence"]["preserved_landing_repository_id"],
        ancestry.repository_id
    );
    assert_eq!(
        finding["evidence"]["observed_repository_id"],
        canonical_repository_id
    );

    let doctor = run_mote(
        &repositories.canonical,
        &repositories.canonical,
        "recovery-operator",
        &["--json", "doctor"],
    );
    assert!(doctor.status.success());
    let doctor: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(doctor["ok"], true);
}

#[test]
fn availability_gate_is_typed_and_parent_anchored() {
    let repositories = repositories();
    let candidate_id = ids::new_candidate_id();
    let source_repo = "repo-source";
    let landing_repo = "repo-landing";
    let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let base = "1111111111111111111111111111111111111111";
    let portable_v1 = CandidateProposeOp {
        v: CANDIDATE_PROTOCOL_VERSION,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.clone(),
        entity: repositories.issue,
        store_id: repositories.store.read_format().unwrap().store_id,
        repository_id: source_repo.into(),
        landing_repository_id: Some(landing_repo.into()),
        object_source: Some(CandidateObjectSource {
            repository_id: source_repo.into(),
            commit_ref: commit.into(),
            locator: Some("path:/source".into()),
        }),
        object_format: "sha1".into(),
        commit_oid: commit.into(),
        base_oid: base.into(),
        parent_oids: vec![base.into()],
        paths: vec!["work.txt".into()],
        authorizer: "authorizer".into(),
        reviewers: vec!["reviewer".into()],
        review_policy: None,
        evidence_requirements: vec![EvidenceRequirement {
            name: GIT_ANCESTRY_EVIDENCE.into(),
            kind: "git".into(),
            producers: vec!["proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: "typed-proposal-v1".into(),
    };
    publish_rejected(
        &repositories.store,
        &Op::CandidatePropose(portable_v1.clone()),
        "unsupported or inconsistent candidate review policy version",
    );
    let mut portable_v2 = portable_v1.clone();
    portable_v2.v = CANDIDATE_ROLE_REVIEW_VERSION;
    portable_v2.reviewers.clear();
    portable_v2.review_policy = Some(CandidateReviewPolicy {
        named_reviewers: vec!["reviewer".into()],
        role_requirements: Vec::new(),
    });
    portable_v2.idempotency_key = "typed-proposal-v2".into();
    publish_rejected(
        &repositories.store,
        &Op::CandidatePropose(portable_v2.clone()),
        "unsupported or inconsistent candidate review policy version",
    );
    let mut portable_v3 = portable_v2;
    portable_v3.v = CANDIDATE_PORTABLE_PROTOCOL_VERSION;
    portable_v3.idempotency_key = "typed-proposal-v3".into();
    publish_checked(&repositories.store, &Op::CandidatePropose(portable_v3));
    let state = reducer::replay_store(&repositories.store).unwrap();
    assert!(
        state
            .candidate_landability(&candidate_id, None)
            .reason_codes
            .contains(&"object_availability_missing".into())
    );

    let unavailable =
        CandidateEvidencePayload::GitObjectAvailability(GitObjectAvailabilityReceipt {
            repository_id: landing_repo.into(),
            object_format: "sha1".into(),
            candidate_oid: commit.into(),
            observed_parent_oids: Vec::new(),
            object_available: Some(false),
            git_version: "git version test".into(),
            detail: Some("missing object".into()),
        });
    publish_checked(
        &repositories.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            candidate_oid: commit.into(),
            evidence_id: mote::candidate::evidence_id(&unavailable).unwrap(),
            name: GIT_OBJECT_AVAILABILITY_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Fail,
            payload: unavailable,
            refs: Vec::new(),
            idempotency_key: "typed-unavailable".into(),
        }),
    );
    assert!(
        reducer::replay_store(&repositories.store)
            .unwrap()
            .candidate_landability(&candidate_id, None)
            .reason_codes
            .contains(&"object_unreachable".into())
    );

    let wrong_parent =
        CandidateEvidencePayload::GitObjectAvailability(GitObjectAvailabilityReceipt {
            repository_id: landing_repo.into(),
            object_format: "sha1".into(),
            candidate_oid: commit.into(),
            observed_parent_oids: vec!["2222222222222222222222222222222222222222".into()],
            object_available: Some(true),
            git_version: "git version test".into(),
            detail: None,
        });
    publish_rejected(
        &repositories.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id,
            candidate_oid: commit.into(),
            evidence_id: mote::candidate::evidence_id(&wrong_parent).unwrap(),
            name: GIT_OBJECT_AVAILABILITY_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload: wrong_parent,
            refs: Vec::new(),
            idempotency_key: "typed-wrong-parent".into(),
        }),
        "immutable object anchors",
    );
}

#[test]
fn landing_repository_binding_is_authorizer_owned_and_compare_and_set() {
    let repositories = repositories();
    let candidate_id = ids::new_candidate_id();
    let source_repo = "repo-legacy-source";
    let commit = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let base = "1111111111111111111111111111111111111111";
    let proposal_op = publish_checked(
        &repositories.store,
        &Op::CandidatePropose(CandidateProposeOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "proposer".into(),
            candidate_id: candidate_id.clone(),
            entity: repositories.issue,
            store_id: repositories.store.read_format().unwrap().store_id,
            repository_id: source_repo.into(),
            landing_repository_id: None,
            object_source: None,
            object_format: "sha1".into(),
            commit_oid: commit.into(),
            base_oid: base.into(),
            parent_oids: vec![base.into()],
            paths: vec!["work.txt".into()],
            authorizer: "authorizer".into(),
            reviewers: vec!["reviewer".into()],
            review_policy: None,
            evidence_requirements: vec![EvidenceRequirement {
                name: GIT_ANCESTRY_EVIDENCE.into(),
                kind: "git".into(),
                producers: vec!["proposer".into()],
            }],
            evidence_refs: Vec::new(),
            idempotency_key: "binding-proposal".into(),
        }),
    );
    let binding = |actor: &str, target: &str, expect: &str, key: &str| {
        Op::CandidateLandingRepositoryBind(CandidateLandingRepositoryBindOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: actor.into(),
            candidate_id: candidate_id.clone(),
            landing_repository_id: target.into(),
            object_source: Some(CandidateObjectSource {
                repository_id: source_repo.into(),
                commit_ref: commit.into(),
                locator: Some("path:/source".into()),
            }),
            expect_phase: proposal_op.clone(),
            expect_landing_repository: expect.into(),
            reason: "bind shared repository".into(),
            idempotency_key: key.into(),
        })
    };
    publish_rejected(
        &repositories.store,
        &binding(
            "proposer",
            "repo-shared",
            &proposal_op,
            "binding-wrong-actor",
        ),
        "proposal authorizer",
    );
    let binding_op = publish_checked(
        &repositories.store,
        &binding(
            "authorizer",
            "repo-shared",
            &proposal_op,
            "binding-accepted",
        ),
    );
    publish_rejected(
        &repositories.store,
        &binding("authorizer", "repo-other", &proposal_op, "binding-stale"),
        "current pending phase and repository CAS",
    );
    let state = reducer::replay_store(&repositories.store).unwrap();
    assert_eq!(
        state.candidates[&candidate_id].landing_repository_id,
        "repo-shared"
    );
    assert_eq!(
        state.candidates[&candidate_id].landing_repository_op_id,
        binding_op
    );
    assert!(state.candidates[&candidate_id].object_availability_required);
}
