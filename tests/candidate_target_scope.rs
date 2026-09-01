use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use jiff::Timestamp;
use tempfile::TempDir;

use mote::candidate::CandidateObjectSource;
use mote::candidate::{
    CandidateEvidencePayload, EvidenceOutcome, GIT_LANDING_EVIDENCE, GIT_TARGET_SCOPE_EVIDENCE,
    GitTargetScopeReceipt,
};
use mote::ids;
use mote::op::{
    CandidateEvidenceOp, CandidateLandedOp, CandidateLandingRepositoryBindOp, Op, ScalarSet,
    make_create,
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

fn run_mote(cwd: &Path, actor: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(cwd)
        .arg("--store")
        .arg(cwd)
        .arg("--actor")
        .arg(actor)
        .args(args)
        .output()
        .unwrap()
}

fn run_mote_json(cwd: &Path, actor: &str, args: &[&str]) -> serde_json::Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let output = run_mote(cwd, actor, &full);
    assert!(
        output.status.success(),
        "mote {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    store: Store,
    issue: String,
    root_commit: String,
}

fn fixture(files: &[(&str, &str)]) -> Fixture {
    let temp = TempDir::new().unwrap();
    let root = temp.path().to_path_buf();
    run_git(&root, &["init", "-q", "-b", "main"]);
    run_git(&root, &["config", "user.email", "test@example.com"]);
    run_git(&root, &["config", "user.name", "Test"]);
    for (path, body) in files {
        std::fs::write(root.join(path), body).unwrap();
        run_git(&root, &["add", path]);
    }
    run_git(&root, &["commit", "-qm", "root"]);
    let root_commit = run_git(&root, &["rev-parse", "HEAD"]);
    let store = Store::init(&root).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("target scope court".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    Fixture {
        _temp: temp,
        root,
        store,
        issue,
        root_commit,
    }
}

fn propose(fixture: &Fixture, base: &str, paths: &[&str], key: &str) -> serde_json::Value {
    let mut owned = vec![
        "candidate".to_string(),
        "propose".to_string(),
        "--issue".to_string(),
        fixture.issue.clone(),
        "--base".to_string(),
        base.to_string(),
        "--path".to_string(),
    ];
    owned.extend(paths.iter().map(|path| (*path).to_string()));
    owned.extend([
        "--authorizer".into(),
        "authorizer".into(),
        "--reviewer".into(),
        "reviewer".into(),
        "--idempotency-key".into(),
        key.into(),
    ]);
    let args = owned.iter().map(String::as_str).collect::<Vec<_>>();
    run_mote_json(&fixture.root, "proposer", &args)
}

fn record_target_scope(
    fixture: &Fixture,
    candidate_id: &str,
    target: &str,
    key: &str,
) -> serde_json::Value {
    run_mote_json(
        &fixture.root,
        "authorizer",
        &[
            "candidate",
            "evidence",
            "target-scope",
            candidate_id,
            "--target",
            target,
            "--idempotency-key",
            key,
        ],
    )
}

fn review_and_authorize(fixture: &Fixture, candidate_id: &str) -> serde_json::Value {
    run_mote_json(
        &fixture.root,
        "reviewer",
        &[
            "candidate",
            "review",
            candidate_id,
            "approve",
            "--idempotency-key",
            "review",
        ],
    );
    run_mote_json(
        &fixture.root,
        "authorizer",
        &[
            "candidate",
            "authorize",
            candidate_id,
            "--grantee",
            "lander",
            "--idempotency-key",
            "authorize",
        ],
    )
}

fn target_scope_receipt(candidate: &serde_json::Value) -> &serde_json::Value {
    candidate["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .find(|evidence| evidence["name"] == GIT_TARGET_SCOPE_EVIDENCE)
        .map(|evidence| &evidence["payload"])
        .unwrap()
}

#[test]
fn direct_target_is_blocked_until_exact_scope_then_becomes_landable() {
    let fixture = fixture(&[("work.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("work.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["commit", "-qam", "candidate"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["work.txt"],
        "propose-direct",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let authorized = review_and_authorize(&fixture, candidate_id);
    assert_eq!(authorized["landability"]["landable"], false);
    assert!(
        authorized["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|code| code == "target_scope_evidence_missing")
    );

    let scoped = record_target_scope(&fixture, candidate_id, "main", "scope-direct");
    assert_eq!(scoped["landability"]["landable"], true);
    let receipt = target_scope_receipt(&scoped);
    assert_eq!(receipt["observed_target_oid"], fixture.root_commit);
    assert_eq!(receipt["target_ref_full_name"], "refs/heads/main");
    assert_eq!(receipt["candidate_is_ancestor_of_target"], false);
    assert_eq!(receipt["base_is_ancestor_of_target"], true);
    assert_eq!(
        receipt["candidate_side_paths"],
        serde_json::json!(["work.txt"])
    );
    assert_eq!(
        receipt["prospective_target_effect_paths"],
        serde_json::json!(["work.txt"])
    );
    assert_eq!(receipt["effective_paths"], serde_json::json!(["work.txt"]));
}

#[test]
fn target_scope_refuses_candidate_already_reachable_from_target() {
    let fixture = fixture(&[("work.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("work.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["commit", "-qam", "candidate"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["work.txt"],
        "propose-already-reachable",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    run_git(&fixture.root, &["switch", "main"]);
    run_git(&fixture.root, &["merge", "--ff-only", "candidate"]);

    let scoped = run_mote(
        &fixture.root,
        "authorizer",
        &[
            "candidate",
            "evidence",
            "target-scope",
            candidate_id,
            "--target",
            "main",
            "--idempotency-key",
            "scope-after-landing",
        ],
    );
    assert!(!scoped.status.success());
    assert!(
        String::from_utf8_lossy(&scoped.stderr).contains("use out-of-band reconciliation instead")
    );
}

#[test]
fn divergent_recorded_base_cannot_hide_candidate_side_paths() {
    let fixture = fixture(&[("root.txt", "root\n")]);
    std::fs::write(fixture.root.join("target.txt"), "target\n").unwrap();
    run_git(&fixture.root, &["add", "target.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "target advance"]);

    run_git(
        &fixture.root,
        &["switch", "-qc", "candidate-base", &fixture.root_commit],
    );
    std::fs::write(fixture.root.join("hidden.txt"), "hidden\n").unwrap();
    run_git(&fixture.root, &["add", "hidden.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate base"]);
    let recorded_base = run_git(&fixture.root, &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root.join("declared.txt"), "declared\n").unwrap();
    run_git(&fixture.root, &["add", "declared.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate tip"]);

    let proposed = propose(
        &fixture,
        &recorded_base,
        &["declared.txt"],
        "propose-divergent",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let scoped = record_target_scope(&fixture, candidate_id, "main", "scope-divergent");
    let receipt = target_scope_receipt(&scoped);
    assert_eq!(receipt["base_is_ancestor_of_target"], false);
    assert_eq!(
        receipt["effective_paths"],
        serde_json::json!(["declared.txt", "hidden.txt"])
    );
    assert!(
        scoped["landability"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| {
                reason["code"] == "target_scope_uncovered"
                    && reason["detail"].as_str().unwrap().contains("hidden.txt")
            })
    );
}

#[test]
fn rename_scope_conservatively_contains_source_and_destination() {
    let fixture = fixture(&[("old.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    run_git(&fixture.root, &["mv", "old.txt", "new.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "rename"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["new.txt"],
        "propose-rename",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let scoped = record_target_scope(&fixture, candidate_id, "main", "scope-rename");
    let receipt = target_scope_receipt(&scoped);
    assert_eq!(
        receipt["effective_paths"],
        serde_json::json!(["new.txt", "old.txt"])
    );
    assert!(
        scoped["landability"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| {
                reason["code"] == "target_scope_uncovered"
                    && reason["detail"].as_str().unwrap().contains("old.txt")
            })
    );
}

#[test]
fn target_side_rename_expands_scope_to_the_actual_destination() {
    let fixture = fixture(&[("old.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("old.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["commit", "-qam", "modify source"]);

    run_git(&fixture.root, &["switch", "main"]);
    run_git(&fixture.root, &["mv", "old.txt", "new.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "rename on target"]);
    run_git(&fixture.root, &["switch", "candidate"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["old.txt"],
        "propose-target-rename",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let scoped = record_target_scope(&fixture, candidate_id, "main", "scope-target-rename");
    let receipt = target_scope_receipt(&scoped);
    assert_eq!(
        receipt["prospective_target_effect_paths"],
        serde_json::json!(["new.txt"])
    );
    assert_eq!(
        receipt["effective_paths"],
        serde_json::json!(["new.txt", "old.txt"])
    );
    assert!(
        scoped["landability"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| {
                reason["code"] == "target_scope_uncovered"
                    && reason["detail"].as_str().unwrap().contains("new.txt")
            })
    );

    run_git(&fixture.root, &["switch", "main"]);
    run_git(
        &fixture.root,
        &["merge", "--no-ff", "-qm", "merge candidate", "candidate"],
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("new.txt")).unwrap(),
        "candidate\n"
    );
}

#[test]
fn target_advancement_invalidates_the_recorded_landing_preimage() {
    let fixture = fixture(&[("root.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("candidate.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["add", "candidate.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate"]);
    let candidate_oid = run_git(&fixture.root, &["rev-parse", "HEAD"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["candidate.txt"],
        "propose-advance",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();
    record_target_scope(&fixture, candidate_id, "main", "scope-before-advance");
    let authorized = review_and_authorize(&fixture, candidate_id);
    assert_eq!(authorized["landability"]["landable"], true);
    let authorization_op = authorized["authorization"]["op_id"].as_str().unwrap();

    run_git(&fixture.root, &["switch", "main"]);
    std::fs::write(fixture.root.join("target.txt"), "advanced\n").unwrap();
    run_git(&fixture.root, &["add", "target.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "advance target"]);
    run_git(
        &fixture.root,
        &["merge", "--no-ff", "-qm", "merge candidate", &candidate_oid],
    );

    let landed = run_mote(
        &fixture.root,
        "lander",
        &[
            "candidate",
            "landed",
            candidate_id,
            "--target",
            "main",
            "--expect-phase",
            phase_op,
            "--expect-authorization",
            authorization_op,
            "--idempotency-key",
            "land-with-stale-scope",
        ],
    );
    assert!(!landed.status.success());
    assert!(String::from_utf8_lossy(&landed.stderr).contains("target-scope evidence is stale"));
    let state = reducer::replay_store(&fixture.store).unwrap();
    assert_eq!(state.candidates[candidate_id].phase.as_str(), "pending");
}

#[test]
fn linear_target_advancement_cannot_reuse_an_older_scope_preimage() {
    let fixture = fixture(&[("root.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("advance.txt"), "advance\n").unwrap();
    run_git(&fixture.root, &["add", "advance.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "linear advance"]);
    let advanced_target_oid = run_git(&fixture.root, &["rev-parse", "HEAD"]);
    std::fs::write(fixture.root.join("candidate.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["add", "candidate.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate tip"]);
    let candidate_oid = run_git(&fixture.root, &["rev-parse", "HEAD"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["advance.txt", "candidate.txt"],
        "propose-linear-advance",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();
    record_target_scope(
        &fixture,
        candidate_id,
        "main",
        "scope-before-linear-advance",
    );
    let authorized = review_and_authorize(&fixture, candidate_id);
    assert_eq!(authorized["landability"]["landable"], true);
    let authorization_op = authorized["authorization"]["op_id"].as_str().unwrap();

    run_git(
        &fixture.root,
        &["branch", "-f", "main", &advanced_target_oid],
    );
    run_git(&fixture.root, &["branch", "-f", "main", &candidate_oid]);

    let landed = run_mote(
        &fixture.root,
        "lander",
        &[
            "candidate",
            "landed",
            candidate_id,
            "--target",
            "main",
            "--expect-phase",
            phase_op,
            "--expect-authorization",
            authorization_op,
            "--idempotency-key",
            "land-after-linear-advance",
        ],
    );
    assert!(!landed.status.success());
    assert!(
        String::from_utf8_lossy(&landed.stderr)
            .contains("immediate target preimage does not match target-scope evidence")
    );
    let state = reducer::replay_store(&fixture.store).unwrap();
    assert_eq!(state.candidates[candidate_id].phase.as_str(), "pending");
}

#[test]
fn landing_result_with_an_unscoped_actual_effect_is_rejected() {
    let fixture = fixture(&[("root.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("candidate.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["add", "candidate.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["candidate.txt"],
        "propose-tampered-result",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();
    record_target_scope(
        &fixture,
        candidate_id,
        "main",
        "scope-before-tampered-result",
    );
    let authorized = review_and_authorize(&fixture, candidate_id);
    assert_eq!(authorized["landability"]["landable"], true);
    let authorization_op = authorized["authorization"]["op_id"].as_str().unwrap();

    run_git(&fixture.root, &["switch", "main"]);
    run_git(
        &fixture.root,
        &["merge", "--no-ff", "--no-commit", "candidate"],
    );
    std::fs::write(fixture.root.join("extra.txt"), "not scoped\n").unwrap();
    run_git(&fixture.root, &["add", "extra.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "tampered merge result"]);

    let landed = run_mote(
        &fixture.root,
        "lander",
        &[
            "candidate",
            "landed",
            candidate_id,
            "--target",
            "main",
            "--expect-phase",
            phase_op,
            "--expect-authorization",
            authorization_op,
            "--idempotency-key",
            "land-tampered-result",
        ],
    );
    assert!(!landed.status.success());
    assert!(
        String::from_utf8_lossy(&landed.stderr)
            .contains("actual target-to-result paths do not match target-scope evidence")
    );
    let state = reducer::replay_store(&fixture.store).unwrap();
    assert_eq!(state.candidates[candidate_id].phase.as_str(), "pending");
}

#[test]
fn landing_result_with_same_path_content_substitution_is_rejected() {
    let fixture = fixture(&[("root.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("candidate.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["add", "candidate.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["candidate.txt"],
        "propose-substituted-result",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap();
    record_target_scope(
        &fixture,
        candidate_id,
        "main",
        "scope-before-substituted-result",
    );
    let authorized = review_and_authorize(&fixture, candidate_id);
    assert_eq!(authorized["landability"]["landable"], true);
    let authorization_op = authorized["authorization"]["op_id"].as_str().unwrap();

    run_git(&fixture.root, &["switch", "main"]);
    run_git(
        &fixture.root,
        &["merge", "--no-ff", "--no-commit", "candidate"],
    );
    std::fs::write(fixture.root.join("candidate.txt"), "substituted\n").unwrap();
    run_git(&fixture.root, &["add", "candidate.txt"]);
    run_git(
        &fixture.root,
        &["commit", "-qm", "substituted merge result"],
    );

    let landed = run_mote(
        &fixture.root,
        "lander",
        &[
            "candidate",
            "landed",
            candidate_id,
            "--target",
            "main",
            "--expect-phase",
            phase_op,
            "--expect-authorization",
            authorization_op,
            "--idempotency-key",
            "land-substituted-result",
        ],
    );
    assert!(!landed.status.success());
    assert!(
        String::from_utf8_lossy(&landed.stderr)
            .contains("actual landing tree does not match target-scope evidence")
    );
    let state = reducer::replay_store(&fixture.store).unwrap();
    assert_eq!(state.candidates[candidate_id].phase.as_str(), "pending");
}

#[test]
fn reducer_rejects_a_landing_tree_that_disagrees_with_scope() {
    let fixture = fixture(&[("root.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("candidate.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["add", "candidate.txt"]);
    run_git(&fixture.root, &["commit", "-qm", "candidate"]);

    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["candidate.txt"],
        "propose-reducer-tree",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let phase_op = proposed["phase"]["op_id"].as_str().unwrap().to_string();
    record_target_scope(&fixture, candidate_id, "main", "scope-reducer-tree");
    let authorized = review_and_authorize(&fixture, candidate_id);
    let authorization_op = authorized["authorization"]["op_id"]
        .as_str()
        .unwrap()
        .to_string();

    let state = reducer::replay_store(&fixture.store).unwrap();
    let candidate = &state.candidates[candidate_id];
    let scope_record = candidate
        .evidence
        .values()
        .find(|evidence| evidence.name == GIT_TARGET_SCOPE_EVIDENCE)
        .unwrap();
    let scope = match &scope_record.payload {
        CandidateEvidencePayload::GitTargetScope(scope) => scope.clone(),
        _ => unreachable!(),
    };
    let scope_evidence_id = scope_record.evidence_id.clone();
    let scope_op_id = scope_record.op_id.clone();
    let landing_repository_id = candidate.landing_repository_id.clone();
    let object_format = candidate.object_format.clone();
    let candidate_oid = candidate.commit_oid.clone();

    run_git(&fixture.root, &["switch", "main"]);
    run_git(&fixture.root, &["merge", "--ff-only", "candidate"]);
    let mut receipt = mote::candidate::probe_landing(
        &fixture.root,
        &landing_repository_id,
        &object_format,
        &candidate_oid,
        "main",
        None,
        &authorization_op,
        Vec::new(),
        Some(&scope),
        Some(&scope_evidence_id),
        Some(&scope_op_id),
    )
    .unwrap();
    let root_tree_expression = format!("{}^{{tree}}", fixture.root_commit);
    receipt.after_tree_oid = Some(run_git(
        &fixture.root,
        &["rev-parse", &root_tree_expression],
    ));
    assert_ne!(
        receipt.after_tree_oid.as_deref(),
        Some(scope.prospective_merge_tree_oid.as_str())
    );

    let payload = CandidateEvidencePayload::GitLanding(receipt);
    let evidence_id = mote::candidate::evidence_id(&payload).unwrap();
    let evidence_op = publish::publish_op(
        &fixture.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "lander".into(),
            candidate_id: candidate_id.into(),
            candidate_oid,
            evidence_id: evidence_id.clone(),
            name: GIT_LANDING_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload,
            refs: Vec::new(),
            idempotency_key: "forged-landing-tree-evidence".into(),
        }),
    )
    .unwrap();
    let landed_op = publish::publish_op(
        &fixture.store,
        &Op::CandidateLanded(CandidateLandedOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "lander".into(),
            candidate_id: candidate_id.into(),
            evidence_id,
            expect_phase: phase_op,
            expect_authorization: authorization_op,
            target_ref: "main".into(),
            idempotency_key: "land-with-forged-tree".into(),
        }),
    )
    .unwrap();

    let replayed = reducer::replay_store(&fixture.store).unwrap();
    assert!(replayed.was_accepted(evidence_op.as_str()));
    assert!(!replayed.was_accepted(landed_op.as_str()));
    assert_eq!(replayed.candidates[candidate_id].phase.as_str(), "pending");
}

#[test]
fn repository_and_binding_identity_are_derived_and_reducer_checked() {
    let fixture = fixture(&[("work.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("work.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["commit", "-qam", "candidate"]);
    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["work.txt"],
        "propose-identity",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    let state = reducer::replay_store(&fixture.store).unwrap();
    let candidate = &state.candidates[candidate_id];

    let wrong_expected_repository = mote::candidate::probe_target_scope(
        &fixture.root,
        "repo-caller-asserted",
        &candidate.landing_repository_op_id,
        &candidate.object_format,
        &candidate.commit_oid,
        &candidate.base_oid,
        "main",
    )
    .unwrap_err();
    assert!(wrong_expected_repository.contains("repository identity does not match"));

    let mut receipt = mote::candidate::probe_target_scope(
        &fixture.root,
        &candidate.landing_repository_id,
        &candidate.landing_repository_op_id,
        &candidate.object_format,
        &candidate.commit_oid,
        &candidate.base_oid,
        "main",
    )
    .unwrap();
    receipt.repository_id = "repo-forged".into();
    let payload = CandidateEvidencePayload::GitTargetScope(receipt);
    let rejected = publish::publish_op(
        &fixture.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.into(),
            candidate_oid: candidate.commit_oid.clone(),
            evidence_id: mote::candidate::evidence_id(&payload).unwrap(),
            name: GIT_TARGET_SCOPE_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload,
            refs: Vec::new(),
            idempotency_key: "forged-repository".into(),
        }),
    )
    .unwrap();
    let replayed = reducer::replay_store(&fixture.store).unwrap();
    assert!(!replayed.was_accepted(rejected.as_str()));
    assert!(
        replayed
            .rejection_reason(rejected.as_str())
            .unwrap()
            .contains("current landing repository, binding")
    );

    let mut stale_binding: GitTargetScopeReceipt = mote::candidate::probe_target_scope(
        &fixture.root,
        &candidate.landing_repository_id,
        &candidate.landing_repository_op_id,
        &candidate.object_format,
        &candidate.commit_oid,
        &candidate.base_oid,
        "main",
    )
    .unwrap();
    stale_binding.landing_repository_op_id = "stale-binding-op".into();
    let payload = CandidateEvidencePayload::GitTargetScope(stale_binding);
    let rejected = publish::publish_op(
        &fixture.store,
        &Op::CandidateEvidence(CandidateEvidenceOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.into(),
            candidate_oid: candidate.commit_oid.clone(),
            evidence_id: mote::candidate::evidence_id(&payload).unwrap(),
            name: GIT_TARGET_SCOPE_EVIDENCE.into(),
            evidence_kind: "git".into(),
            producer_tool: "git version test".into(),
            outcome: EvidenceOutcome::Pass,
            payload,
            refs: Vec::new(),
            idempotency_key: "stale-binding".into(),
        }),
    )
    .unwrap();
    let replayed = reducer::replay_store(&fixture.store).unwrap();
    assert!(!replayed.was_accepted(rejected.as_str()));
}

#[test]
fn landing_repository_binding_advance_makes_prior_scope_stale() {
    let fixture = fixture(&[("work.txt", "root\n")]);
    run_git(&fixture.root, &["switch", "-qc", "candidate"]);
    std::fs::write(fixture.root.join("work.txt"), "candidate\n").unwrap();
    run_git(&fixture.root, &["commit", "-qam", "candidate"]);
    let proposed = propose(
        &fixture,
        &fixture.root_commit,
        &["work.txt"],
        "propose-stale-binding",
    );
    let candidate_id = proposed["candidate_id"].as_str().unwrap();
    record_target_scope(&fixture, candidate_id, "main", "scope-before-rebind");

    let state = reducer::replay_store(&fixture.store).unwrap();
    let candidate = &state.candidates[candidate_id];
    let binding_op = candidate.landing_repository_op_id.clone();
    let phase_op = candidate.phase_op_id.clone();
    let repository_id = candidate.landing_repository_id.clone();
    let source_repository_id = candidate.repository_id.clone();
    let commit_oid = candidate.commit_oid.clone();
    let rebound = publish::publish_op(
        &fixture.store,
        &Op::CandidateLandingRepositoryBind(CandidateLandingRepositoryBindOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "authorizer".into(),
            candidate_id: candidate_id.into(),
            landing_repository_id: repository_id,
            object_source: Some(CandidateObjectSource {
                repository_id: source_repository_id,
                commit_ref: commit_oid,
                locator: Some("path:/alternate-object-source".into()),
            }),
            expect_phase: phase_op,
            expect_landing_repository: binding_op,
            reason: "advance the landing binding clock".into(),
            idempotency_key: "advance-binding".into(),
        }),
    )
    .unwrap();

    let replayed = reducer::replay_store(&fixture.store).unwrap();
    assert!(replayed.was_accepted(rebound.as_str()));
    let shown = run_mote_json(
        &fixture.root,
        "authorizer",
        &["candidate", "show", candidate_id],
    );
    assert!(
        shown["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|code| code == "target_scope_evidence_stale")
    );
}
