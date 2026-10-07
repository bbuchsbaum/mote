//! Fenced, fast-forward Git landing with a durable prepare/confirm journal.
//! The shared store lock covers authorization, Git CAS, and confirmation. A
//! prepared journal retains that authority across process death: other writers
//! must wait for this exact request to recover, including after lease expiry.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

use crate::{
    MoteError, MoteResult, Store,
    authority::{self, PreparedOp, Writer},
    candidate::{self, CandidateEvidencePayload, EvidenceOutcome},
    ids, op, reducer,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub actor: String,
    pub candidate_id: String,
    pub target: String,
    pub before: String,
    pub expect_phase: String,
    pub expect_authorization: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum Phase {
    Prepared,
    Updating,
    Updated,
    Confirmed,
    Aborted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Journal {
    request: Request,
    repository_id: String,
    object_format: String,
    target_ref: String,
    new_oid: String,
    /// Complete admitted prefix at the authority decision, including policy,
    /// review, evidence and role/lease state. Its bytes are hash-bound by the ledger.
    basis: Vec<String>,
    prepared_at: String,
    reflog_message: String,
    evidence: PreparedOp,
    landed: PreparedOp,
    phase: Phase,
    detail: Option<String>,
}

fn git(cwd: &Path, args: &[&str]) -> MoteResult<String> {
    let output = Command::new("git").current_dir(cwd).args(args).output()?;
    if !output.status.success() {
        return Err(MoteError::Rejected(format!(
            "git {}: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

fn current(cwd: &Path, target: &str) -> Option<String> {
    git(cwd, &["rev-parse", "--verify", target]).ok()
}

fn archive_path(store: &Store, request: &Request) -> std::path::PathBuf {
    let key =
        serde_json::to_vec(&(&request.actor, &request.idempotency_key)).expect("string tuple");
    authority::directory(store).join(format!("landing-{}.json", blake3::hash(&key).to_hex()))
}

fn prepare(store: &Store, cwd: &Path, request: Request) -> MoteResult<Journal> {
    let state = reducer::replay_store(store)?;
    let candidate = state
        .candidates
        .get(&request.candidate_id)
        .ok_or_else(|| MoteError::Rejected("candidate does not exist".into()))?;
    let now = ids::format_rfc3339(Timestamp::now());
    if candidate.phase_op_id != request.expect_phase
        || candidate.authorization.as_ref().map(|a| &a.op_id) != Some(&request.expect_authorization)
    {
        return Err(MoteError::Rejected(
            "phase or authorization precondition changed".into(),
        ));
    }
    let landability =
        state.candidate_landability_at(&request.candidate_id, Some(&request.actor), &now);
    if !landability.landable {
        return Err(MoteError::Rejected(format!(
            "candidate is not landable: {}",
            landability.reason_codes.join(",")
        )));
    }
    let (repository_id, object_format, _) =
        candidate::repository_identity(cwd).map_err(candidate::git_probe_error)?;
    if repository_id != candidate.landing_repository_id || object_format != candidate.object_format
    {
        return Err(MoteError::Rejected(
            "landing repository identity mismatch".into(),
        ));
    }
    let record = candidate
        .evidence
        .values()
        .filter(|e| e.name == candidate::GIT_TARGET_SCOPE_EVIDENCE)
        .max_by(|a, b| a.op_id.cmp(&b.op_id))
        .ok_or_else(|| MoteError::Rejected("exact target-scope evidence required".into()))?;
    let CandidateEvidencePayload::GitTargetScope(scope) = &record.payload else {
        return Err(MoteError::Rejected("invalid target-scope evidence".into()));
    };
    if record.outcome != EvidenceOutcome::Pass
        || scope.target_ref != request.target
        || scope.observed_target_oid != request.before
    {
        return Err(MoteError::Rejected(
            "target/preimage does not match passing target-scope evidence".into(),
        ));
    }
    let fresh = candidate::probe_target_scope(
        cwd,
        &repository_id,
        &candidate.landing_repository_op_id,
        &object_format,
        &candidate.commit_oid,
        &candidate.base_oid,
        &request.target,
    )
    .map_err(candidate::git_probe_error)?;
    if &fresh != scope
        || !candidate::uncovered_target_scope_paths(&candidate.paths, &fresh.effective_paths)
            .is_empty()
    {
        return Err(MoteError::Rejected(
            "target-scope observation changed".into(),
        ));
    }
    let target_ref = fresh.target_ref_full_name.clone();
    if !target_ref.starts_with("refs/heads/") {
        return Err(MoteError::Invalid(
            "fenced landing requires a branch ref".into(),
        ));
    }
    if Command::new("git")
        .current_dir(cwd)
        .args(["symbolic-ref", "-q", &target_ref])
        .output()?
        .status
        .success()
    {
        return Err(MoteError::Invalid(
            "fenced landing does not accept a symbolic target ref".into(),
        ));
    }
    // This primitive changes exactly the candidate commit into the target. A
    // merge result must itself be proposed/reviewed as an immutable candidate.
    git(
        cwd,
        &[
            "merge-base",
            "--is-ancestor",
            &request.before,
            &candidate.commit_oid,
        ],
    )?;
    let tree = git(
        cwd,
        &["rev-parse", &format!("{}^{{tree}}", candidate.commit_oid)],
    )?;
    if tree != fresh.prospective_merge_tree_oid || request.before == candidate.commit_oid {
        return Err(MoteError::Rejected(
            "landing must be an exact nonempty fast-forward".into(),
        ));
    }
    let basis = store.list_op_filenames()?;
    let payload = CandidateEvidencePayload::GitLanding(candidate::GitLandingReceipt {
        repository_id: repository_id.clone(),
        object_format: object_format.clone(),
        candidate_oid: candidate.commit_oid.clone(),
        target_ref: request.target.clone(),
        target_ref_full_name: Some(target_ref.clone()),
        before_tip: Some(request.before.clone()),
        after_tip: candidate.commit_oid.clone(),
        after_tree_oid: Some(tree),
        landing_effect_paths: fresh.prospective_target_effect_paths.clone(),
        candidate_reachable: Some(true),
        authorization_op_id: request.expect_authorization.clone(),
        basis_op_ids: basis
            .iter()
            .map(|s| s.trim_end_matches(".json").into())
            .collect(),
        target_scope_evidence_id: Some(record.evidence_id.clone()),
        target_scope_op_id: Some(record.op_id.clone()),
        git_version: fresh.git_version.clone(),
        detail: Some("Mote fenced ref CAS; published only after Git update".into()),
    });
    let evidence_id = candidate::evidence_id(&payload)?;
    let key_hash = blake3::hash(&serde_json::to_vec(&(
        &request.actor,
        &request.idempotency_key,
    ))?)
    .to_hex()
    .to_string();
    let evidence = PreparedOp::new(&op::Op::CandidateEvidence(op::CandidateEvidenceOp {
        v: 1,
        op: String::new(),
        ts: now.clone(),
        actor: request.actor.clone(),
        candidate_id: request.candidate_id.clone(),
        candidate_oid: candidate.commit_oid.clone(),
        evidence_id: evidence_id.clone(),
        name: candidate::GIT_LANDING_EVIDENCE.into(),
        evidence_kind: "git".into(),
        producer_tool: fresh.git_version,
        outcome: EvidenceOutcome::Pass,
        payload,
        refs: Vec::new(),
        idempotency_key: format!("fenced-evidence-{key_hash}"),
    }))?;
    let landed = PreparedOp::new(&op::Op::CandidateLanded(op::CandidateLandedOp {
        v: 1,
        op: String::new(),
        ts: now.clone(),
        actor: request.actor.clone(),
        candidate_id: request.candidate_id.clone(),
        evidence_id,
        expect_phase: request.expect_phase.clone(),
        expect_authorization: request.expect_authorization.clone(),
        target_ref: request.target.clone(),
        idempotency_key: format!("fenced-landed-{key_hash}"),
    }))?;
    // Refuse before touching Git if either exact, frozen confirmation would be
    // rejected. Replay uses the decision timestamp, including for expiring roles.
    let mut preview = state.clone();
    for prepared in [&evidence, &landed] {
        reducer::apply_entry(
            &mut preview,
            &format!("{}.json", prepared.name),
            &prepared.bytes,
        );
        if !preview.was_accepted(&prepared.name) {
            return Err(MoteError::Rejected(
                preview
                    .rejection_reason(&prepared.name)
                    .unwrap_or_else(|| "confirmation preflight failed".into()),
            ));
        }
    }
    Ok(Journal {
        request,
        repository_id,
        object_format,
        target_ref,
        new_oid: candidate.commit_oid.clone(),
        basis,
        prepared_at: now,
        reflog_message: format!("mote fenced landing {key_hash}"),
        evidence,
        landed,
        phase: Phase::Prepared,
        detail: None,
    })
}

fn update_seen(cwd: &Path, journal: &Journal) -> MoteResult<bool> {
    let log = git(
        cwd,
        &["reflog", "show", "--format=%H%x00%gs", &journal.target_ref],
    )?;
    Ok(log.lines().any(|line| {
        line.split_once('\0').is_some_and(|(oid, message)| {
            oid == journal.new_oid && message == journal.reflog_message
        })
    }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiptContext {
    CurrentCompletion,
    ArchivedReceipt,
}

fn result(
    cwd: &Path,
    journal: &Journal,
    path: &Path,
    retry: bool,
    context: ReceiptContext,
) -> (i32, Value) {
    let oid = current(cwd, &journal.target_ref);
    let seen = update_seen(cwd, journal).ok();
    let updated = match journal.phase {
        Phase::Updated | Phase::Confirmed => Some(true),
        Phase::Prepared | Phase::Aborted => Some(false),
        Phase::Updating => {
            if seen == Some(true) {
                Some(true)
            } else {
                None
            }
        }
    };
    let terminally_confirmed = journal.phase == Phase::Confirmed;
    let target_current = oid.as_deref() == Some(journal.new_oid.as_str());
    let current_completion = context == ReceiptContext::CurrentCompletion;
    // A current completion is successful only after its exact ref observation.
    // An archived receipt describes a completed prior attempt; preserve any
    // recorded cleanup/drift error instead of retrospectively claiming it won.
    let succeeded = terminally_confirmed && target_current && journal.detail.is_none();
    let outcome = if !current_completion && terminally_confirmed {
        "historically_confirmed"
    } else if succeeded {
        "landed"
    } else if journal.phase == Phase::Aborted {
        "aborted"
    } else {
        "recovery_required"
    };
    (
        if succeeded { 0 } else { 2 },
        json!({
            "outcome": outcome,
            "git_updated": updated, "git_updated_unknown": updated.is_none(),
            "old_oid": journal.request.before, "new_oid": journal.new_oid, "current_oid": oid,
            "target_current": target_current,
            "candidate_id": journal.request.candidate_id, "actor": journal.request.actor,
            "target_ref": journal.target_ref, "repository_id": journal.repository_id,
            "journal": path, "phase": journal.phase, "detail": journal.detail, "retry": retry,
            "receipt_context": if current_completion { "current_completion" } else { "archived_receipt" },
            "idempotency_key": journal.request.idempotency_key,
        }),
    )
}

fn finish(writer: &Writer<'_>, cwd: &Path, journal: &mut Journal, active: &Path) -> MoteResult<()> {
    let (repository, format, _) =
        candidate::repository_identity(cwd).map_err(candidate::git_probe_error)?;
    if repository != journal.repository_id || format != journal.object_format {
        return Err(MoteError::Rejected(
            "recovery repository does not match journal".into(),
        ));
    }
    if matches!(journal.phase, Phase::Prepared | Phase::Updating) {
        let seen = update_seen(cwd, journal)?;
        if seen {
            journal.phase = Phase::Updated;
            authority::write_json(active, journal)?;
        } else {
            if current(cwd, &journal.target_ref).as_deref() != Some(&journal.request.before) {
                if journal.phase == Phase::Prepared {
                    // No Git attempt has started. A moved preimage safely aborts
                    // this prepare and releases the store after archiving it.
                    journal.phase = Phase::Aborted;
                    journal.detail = Some("target changed before the Git attempt".into());
                    authority::write_json(active, journal)?;
                    return Ok(());
                }
                // An interrupted attempt has no matching receipt. Preserve the
                // unknown outcome; never reset Git or invent confirmation.
                return Err(MoteError::Rejected(
                    "target changed; no matching landing reflog receipt".into(),
                ));
            }
            journal.phase = Phase::Updating;
            authority::write_json(active, journal)?;
            authority::checkpoint("landing-before-update")?;
            let update = Command::new("git")
                .current_dir(cwd)
                .args([
                    "update-ref",
                    "--no-deref",
                    "--create-reflog",
                    "-m",
                    &journal.reflog_message,
                    &journal.target_ref,
                    &journal.new_oid,
                    &journal.request.before,
                ])
                .output()?;
            if !update.status.success() {
                let seen = update_seen(cwd, journal).ok();
                let tip = current(cwd, &journal.target_ref);
                journal.phase = if seen == Some(true) {
                    Phase::Updated
                } else if seen == Some(false) && tip.as_deref() == Some(&journal.request.before) {
                    Phase::Aborted
                } else {
                    Phase::Updating
                };
                journal.detail = Some(format!(
                    "Git CAS failed: {}",
                    String::from_utf8_lossy(&update.stderr).trim()
                ));
                authority::write_json(active, journal)?;
                if journal.phase == Phase::Aborted {
                    return Ok(());
                }
                return Err(MoteError::Other(journal.detail.clone().unwrap_or_default()));
            }
            authority::checkpoint("landing-after-update")?;
            journal.phase = Phase::Updated;
            authority::write_json(active, journal)?;
        }
    }
    if journal.phase == Phase::Updated {
        if current(cwd, &journal.target_ref).as_deref() != Some(&journal.new_oid) {
            return Err(MoteError::Rejected(
                "Git was updated but target moved before confirmation".into(),
            ));
        }
        authority::checkpoint("landing-updated")?;
        for (prepared, checkpoint) in [
            (&journal.evidence, "landing-evidence"),
            (&journal.landed, "landing-confirmed-op"),
        ] {
            let state = reducer::replay_store(writer.store)?;
            if !state.was_accepted(&prepared.name) {
                if state.rejection_reason(&prepared.name).is_some() {
                    return Err(MoteError::Rejected(format!(
                        "Git updated but confirmation {} rejected",
                        prepared.name
                    )));
                }
                writer.publish(prepared)?;
                let state = reducer::replay_store(writer.store)?;
                if !state.was_accepted(&prepared.name) {
                    return Err(MoteError::Rejected(format!(
                        "Git updated but confirmation rejected: {:?}",
                        state.rejection_reason(&prepared.name)
                    )));
                }
            }
            authority::checkpoint(checkpoint)?;
        }
        journal.phase = Phase::Confirmed;
        authority::write_json(active, journal)?;
    }
    Ok(())
}

/// Execute or recover the byte-identical request. Every post-prepare failure is
/// returned as structured recovery state, including exact Git OIDs and journal.
pub fn execute(store: &Store, cwd: &Path, request: Request) -> MoteResult<(i32, Value)> {
    match execute_inner(store, cwd, request.clone()) {
        Ok(value) => Ok(value),
        Err(error) => {
            // Even lock/publication recovery failures can follow an earlier Git
            // update. Inspect the durable journal before describing the outcome.
            for path in [
                archive_path(store, &request),
                authority::directory(store).join("landing-active.json"),
            ] {
                if path.exists() {
                    match fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<Journal>(&bytes).ok())
                    {
                        Some(mut journal) => {
                            journal.detail = Some(error.to_string());
                            let (_, mut value) = result(
                                cwd,
                                &journal,
                                &path,
                                true,
                                ReceiptContext::CurrentCompletion,
                            );
                            value["outcome"] = json!("recovery_required");
                            return Ok((2, value));
                        }
                        None => {
                            return Ok((
                                2,
                                json!({"outcome": "recovery_required", "git_updated": null,
                            "git_updated_unknown": true, "journal": path, "detail": error.to_string()}),
                            ));
                        }
                    }
                }
            }
            Ok((
                2,
                json!({"outcome": "aborted", "git_updated": false, "git_updated_unknown": false,
                "old_oid": request.before, "new_oid": null, "current_oid": current(cwd, &request.target),
                "candidate_id": request.candidate_id, "detail": error.to_string()}),
            ))
        }
    }
}

fn execute_inner(store: &Store, cwd: &Path, request: Request) -> MoteResult<(i32, Value)> {
    if !op::validate_idempotency_key(&request.idempotency_key) {
        return Err(MoteError::Invalid("invalid idempotency key".into()));
    }
    let writer = Writer::acquire(store)?;
    writer.enable()?;
    let active = authority::directory(store).join("landing-active.json");
    let archive = archive_path(store, &request);
    if archive.exists() {
        let journal: Journal = serde_json::from_slice(&fs::read(&archive)?)?;
        if journal.request != request {
            return Err(MoteError::Rejected(
                "idempotency conflict: landing arguments changed".into(),
            ));
        }
        let (repository, format, _) =
            candidate::repository_identity(cwd).map_err(candidate::git_probe_error)?;
        if repository != journal.repository_id || format != journal.object_format {
            return Err(MoteError::Rejected(
                "retry repository does not match journal".into(),
            ));
        }
        if active.exists() {
            let pending: Journal = serde_json::from_slice(&fs::read(&active)?)?;
            if pending.request != request {
                return Err(MoteError::Rejected(format!(
                    "another landing requires recovery: {}",
                    active.display()
                )));
            }
            if matches!(pending.phase, Phase::Confirmed | Phase::Aborted) {
                // The archive can have reached disk before active cleanup. Keep
                // this retry in current-completion context until both durable
                // records agree and the active barrier is removed.
                authority::write_json(&archive, &pending)?;
                fs::remove_file(&active)?;
                crate::publish::fsync_dir(&authority::directory(store))?;
                return Ok(result(
                    cwd,
                    &pending,
                    &archive,
                    true,
                    ReceiptContext::CurrentCompletion,
                ));
            }
            return Err(MoteError::Rejected(format!(
                "landing recovery remains active: {}",
                active.display()
            )));
        }
        return Ok(result(
            cwd,
            &journal,
            &archive,
            true,
            ReceiptContext::ArchivedReceipt,
        ));
    }
    let retry = active.exists();
    let mut journal = if retry {
        let journal: Journal = serde_json::from_slice(&fs::read(&active)?)?;
        if journal.request != request {
            return Err(MoteError::Rejected(format!(
                "another landing requires recovery: {}",
                active.display()
            )));
        }
        journal
    } else {
        authority::checkpoint("landing-before-read")?;
        let journal = prepare(store, cwd, request)?;
        authority::write_json(&active, &journal)?;
        journal
    };
    let attempt = authority::checkpoint("landing-prepared")
        .and_then(|()| finish(&writer, cwd, &mut journal, &active));
    if let Err(error) = attempt {
        journal.detail = Some(error.to_string());
        // Best effort only: the original durable prepare still proves recovery
        // is required if this write also fails. Never discard it or reset Git.
        let _ = authority::write_json(&active, &journal);
        return Ok(result(
            cwd,
            &journal,
            &active,
            retry,
            ReceiptContext::CurrentCompletion,
        ));
    }
    // A retry that completed the previously interrupted confirmation has a new
    // successful execution result. Persist that fact before archiving so its
    // old error cannot turn a completed retry into a historical failure.
    if retry && journal.phase == Phase::Confirmed && journal.detail.is_some() {
        journal.detail = None;
        if let Err(error) = authority::write_json(&active, &journal) {
            journal.detail = Some(error.to_string());
            let _ = authority::write_json(&active, &journal);
            return Ok(result(
                cwd,
                &journal,
                &active,
                retry,
                ReceiptContext::CurrentCompletion,
            ));
        }
    }
    if let Err(error) = authority::write_json(&archive, &journal)
        .and_then(|()| authority::checkpoint("landing-archived"))
        .and_then(|()| fs::remove_file(&active).map_err(MoteError::Io))
        .and_then(|()| crate::publish::fsync_dir(&authority::directory(store)))
    {
        journal.detail = Some(error.to_string());
        let (_, mut value) = result(
            cwd,
            &journal,
            &active,
            retry,
            ReceiptContext::CurrentCompletion,
        );
        value["outcome"] = json!("recovery_required");
        return Ok((2, value));
    }
    Ok(result(
        cwd,
        &journal,
        &archive,
        retry,
        ReceiptContext::CurrentCompletion,
    ))
}
