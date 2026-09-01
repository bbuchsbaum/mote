//! Read-only operational audit and Git-backed store visibility diagnostics.
//!
//! Reducer state remains authoritative for recorded facts. This module adds
//! explicitly sourced filesystem, Git, and query-time observations without
//! publishing operations or changing reducer semantics.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use jiff::Timestamp;
use serde::Serialize;
use serde_json::{Value, json};

use crate::actor_status;
use crate::candidate::{CandidatePhase, CandidateSupersessionAuthority, GitRelationKind};
use crate::errors::MoteResult;
use crate::ids;
use crate::op::Status;
use crate::repo::Store;
use crate::state::{LeaseDisposition, RequestState, State};

pub const AUDIT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitBackingMode {
    GitBacked,
    LocalUntracked,
    NotInGit,
    Unavailable,
}

impl GitBackingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GitBacked => "git_backed",
            Self::LocalUntracked => "local_untracked",
            Self::NotInGit => "not_in_git",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GitBackingReport {
    pub mode: GitBackingMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub tracked_op_count: usize,
    pub working_op_count: usize,
    pub uncommitted_op_count: usize,
    pub untracked_op_count: usize,
    pub modified_op_count: usize,
    pub staged_op_count: usize,
    pub deleted_op_count: usize,
    pub unparseable_uncommitted_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_uncommitted_op_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_uncommitted_age_s: Option<u64>,
    pub as_of_ts: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip)]
    pub uncommitted_op_ids: BTreeSet<String>,
}

impl GitBackingReport {
    fn empty(mode: GitBackingMode, as_of: Timestamp, detail: Option<String>) -> Self {
        Self {
            mode,
            repository_root: None,
            head: None,
            tracked_op_count: 0,
            working_op_count: 0,
            uncommitted_op_count: 0,
            untracked_op_count: 0,
            modified_op_count: 0,
            staged_op_count: 0,
            deleted_op_count: 0,
            unparseable_uncommitted_count: 0,
            oldest_uncommitted_op_ts: None,
            oldest_uncommitted_age_s: None,
            as_of_ts: ids::format_rfc3339(as_of),
            detail,
            uncommitted_op_ids: BTreeSet::new(),
        }
    }

    pub fn warning(&self) -> Option<String> {
        (self.mode == GitBackingMode::GitBacked && self.uncommitted_op_count > 0).then(|| {
            let age = self
                .oldest_uncommitted_age_s
                .map(format_duration)
                .unwrap_or_else(|| "unknown age".into());
            format!(
                "git-backed op store has {} uncommitted operation(s); oldest is {age}; clones populated from Git cannot observe them; synchronize the op files before asking those producers to refresh",
                self.uncommitted_op_count
            )
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Error,
    Warning,
    Info,
}

impl AuditSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Error => 0,
            Self::Warning => 1,
            Self::Info => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditFailOn {
    Error,
    Warning,
    Never,
}

impl AuditFailOn {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "error" => Some(Self::Error),
            "warning" => Some(Self::Warning),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Never => "never",
        }
    }

    fn triggered_by(self, severity: AuditSeverity) -> bool {
        match self {
            Self::Error => severity == AuditSeverity::Error,
            Self::Warning => severity != AuditSeverity::Info,
            Self::Never => false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditRemediation {
    pub responsible_surface: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditFinding {
    pub code: String,
    pub severity: AuditSeverity,
    pub scope: String,
    pub subject: String,
    pub message: String,
    pub evidence: Value,
    pub remediation: AuditRemediation,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditSummary {
    pub error: usize,
    pub warning: usize,
    pub info: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditContext {
    pub store_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_oid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditReport {
    pub schema_version: u32,
    pub ok: bool,
    pub as_of_ts: String,
    pub fail_on: String,
    pub context: AuditContext,
    pub git_backing: GitBackingReport,
    pub summary: AuditSummary,
    pub findings: Vec<AuditFinding>,
}

impl AuditReport {
    pub fn exit_code(&self) -> i32 {
        if self.ok { 0 } else { 2 }
    }
}

fn run_git(dir: &Path, args: &[&str]) -> Result<Output, String> {
    Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| format!("git {} failed to start: {error}", args.join(" ")))
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = run_git(dir, args)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|error| format!("git {} returned non-UTF-8 output: {error}", args.join(" ")))
}

fn op_id_from_path(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.ends_with(".json"))
        .map(|name| name.trim_end_matches(".json").to_string())
}

fn basic_op_ts_to_timestamp(value: &str) -> Option<Timestamp> {
    if value.len() != 23 {
        return None;
    }
    format!(
        "{}-{}-{}T{}:{}:{}.{}Z",
        &value[0..4],
        &value[4..6],
        &value[6..8],
        &value[9..11],
        &value[11..13],
        &value[13..15],
        &value[16..22]
    )
    .parse()
    .ok()
}

fn timestamp_for_op_id(op_id: &str) -> Option<Timestamp> {
    ids::parse(op_id)
        .ok()
        .and_then(|parts| basic_op_ts_to_timestamp(&parts.ts))
}

pub fn inspect_git_backing(store: &Store, as_of: Timestamp) -> GitBackingReport {
    let Some(store_parent) = store.root().parent() else {
        return GitBackingReport::empty(
            GitBackingMode::NotInGit,
            as_of,
            Some("store has no parent directory".into()),
        );
    };
    let top = match git_text(store_parent, &["rev-parse", "--show-toplevel"]) {
        Ok(top) => PathBuf::from(top),
        Err(error) if error.to_ascii_lowercase().contains("not a git repository") => {
            return GitBackingReport::empty(GitBackingMode::NotInGit, as_of, None);
        }
        Err(error) => {
            return GitBackingReport::empty(GitBackingMode::Unavailable, as_of, Some(error));
        }
    };
    let canonical_top = match std::fs::canonicalize(&top) {
        Ok(path) => path,
        Err(error) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(format!("cannot canonicalize Git root: {error}")),
            );
        }
    };
    let canonical_ops = match std::fs::canonicalize(store.ops_dir()) {
        Ok(path) => path,
        Err(error) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(format!("cannot canonicalize op directory: {error}")),
            );
        }
    };
    let relative_ops = match canonical_ops.strip_prefix(&canonical_top) {
        Ok(path) => path.to_path_buf(),
        Err(_) => {
            return GitBackingReport::empty(
                GitBackingMode::NotInGit,
                as_of,
                Some("operation directory is outside the containing Git worktree".into()),
            );
        }
    };
    let working_names: BTreeSet<String> = match store.list_op_filenames() {
        Ok(names) => names
            .into_iter()
            .filter_map(|name| op_id_from_path(&name))
            .collect(),
        Err(error) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(format!("cannot list operation files: {error}")),
            );
        }
    };

    let tracked_output = match Command::new("git")
        .args(["-C"])
        .arg(&canonical_top)
        .args(["ls-files", "-z", "--"])
        .arg(&relative_ops)
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
            );
        }
        Err(error) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(format!("git ls-files failed to start: {error}")),
            );
        }
    };
    let tracked_names: BTreeSet<String> = tracked_output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter_map(op_id_from_path)
        .collect();

    let mut untracked_names: BTreeSet<String> =
        working_names.difference(&tracked_names).cloned().collect();
    let mut modified_names = BTreeSet::new();
    let mut staged_names = BTreeSet::new();
    let mut deleted_names = BTreeSet::new();
    let status_output = Command::new("git")
        .args(["-C"])
        .arg(&canonical_top)
        .args([
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=no",
            "--",
        ])
        .arg(&relative_ops)
        .output();
    match status_output {
        Ok(output) if output.status.success() => {
            for entry in output.stdout.split(|byte| *byte == 0) {
                if entry.len() < 4 || entry[2] != b' ' {
                    continue;
                }
                let Some(path) = std::str::from_utf8(&entry[3..]).ok() else {
                    continue;
                };
                let Some(op_id) = op_id_from_path(path) else {
                    continue;
                };
                let index = entry[0] as char;
                let worktree = entry[1] as char;
                if index == 'D' || worktree == 'D' {
                    deleted_names.insert(op_id.clone());
                } else if worktree != ' ' {
                    modified_names.insert(op_id.clone());
                }
                if index != ' ' {
                    staged_names.insert(op_id);
                }
            }
        }
        Ok(output) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(String::from_utf8_lossy(&output.stderr).trim().to_string()),
            );
        }
        Err(error) => {
            return GitBackingReport::empty(
                GitBackingMode::Unavailable,
                as_of,
                Some(format!("git status failed to start: {error}")),
            );
        }
    }

    // Files staged as additions are now in the index and should not also be
    // called untracked. The set difference normally handles this, but keep the
    // categories explicitly disjoint for unusual Git index states.
    for staged in &staged_names {
        untracked_names.remove(staged);
    }
    let uncommitted_op_ids: BTreeSet<String> = untracked_names
        .iter()
        .chain(modified_names.iter())
        .chain(staged_names.iter())
        .chain(deleted_names.iter())
        .cloned()
        .collect();
    let mut parsed: Vec<Timestamp> = uncommitted_op_ids
        .iter()
        .filter_map(|op_id| timestamp_for_op_id(op_id))
        .collect();
    parsed.sort();
    let oldest = parsed.first().copied();
    let oldest_uncommitted_age_s = oldest.map(|timestamp| {
        as_of
            .as_second()
            .saturating_sub(timestamp.as_second())
            .max(0) as u64
    });
    let head = git_text(&canonical_top, &["rev-parse", "HEAD"])
        .ok()
        .filter(|head| !head.is_empty());
    let mode = if tracked_names.is_empty() {
        GitBackingMode::LocalUntracked
    } else {
        GitBackingMode::GitBacked
    };

    let unparseable_uncommitted_count = uncommitted_op_ids.len() - parsed.len();
    GitBackingReport {
        mode,
        repository_root: Some(canonical_top.display().to_string()),
        head,
        tracked_op_count: tracked_names.len(),
        working_op_count: working_names.len(),
        uncommitted_op_count: uncommitted_op_ids.len(),
        untracked_op_count: untracked_names.len(),
        modified_op_count: modified_names.len(),
        staged_op_count: staged_names.len(),
        deleted_op_count: deleted_names.len(),
        unparseable_uncommitted_count,
        oldest_uncommitted_op_ts: oldest.map(ids::format_rfc3339),
        oldest_uncommitted_age_s,
        as_of_ts: ids::format_rfc3339(as_of),
        detail: (unparseable_uncommitted_count > 0).then(|| {
            format!(
                "{unparseable_uncommitted_count} uncommitted operation filename(s) have no parseable operation timestamp"
            )
        }),
        uncommitted_op_ids,
    }
}

fn format_duration(total_seconds: u64) -> String {
    let days = total_seconds / 86_400;
    let hours = total_seconds % 86_400 / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    if days > 0 {
        format!("{days}d{hours:02}h{minutes:02}m")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[allow(clippy::too_many_arguments)]
fn finding(
    code: &str,
    severity: AuditSeverity,
    scope: &str,
    subject: impl Into<String>,
    message: impl Into<String>,
    evidence: Value,
    responsible_surface: &str,
    remediation: impl Into<String>,
) -> AuditFinding {
    AuditFinding {
        code: code.into(),
        severity,
        scope: scope.into(),
        subject: subject.into(),
        message: message.into(),
        evidence,
        remediation: AuditRemediation {
            responsible_surface: responsible_surface.into(),
            text: remediation.into(),
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run_audit(
    store: &Store,
    state: &State,
    repo_cwd: &Path,
    as_of: Timestamp,
    target_ref: Option<&str>,
    stale_after_s: Option<u32>,
    fail_on: AuditFailOn,
) -> MoteResult<AuditReport> {
    let format = store.read_format()?;
    let as_of_ts = ids::format_rfc3339(as_of);
    let git_backing = inspect_git_backing(store, as_of);
    let mut findings = Vec::new();
    let mut skipped = 0;

    if let Some(message) = git_backing.warning() {
        findings.push(finding(
            "git_store_uncommitted_ops",
            AuditSeverity::Warning,
            "store",
            &format.store_id,
            message,
            serde_json::to_value(&git_backing)?,
            "store_sync",
            "synchronize the immutable op files; audit will not commit or push them",
        ));
    }

    for role in state.roles.values().filter(|role| role.retired.is_none()) {
        let coverage = state
            .role_coverage(&role.role_id, &as_of_ts)
            .expect("known role has coverage");
        if coverage.coverage_shortfall == 0 {
            continue;
        }
        let assignments = state
            .role_assignments_for(&role.role_id)
            .into_iter()
            .map(|assignment| {
                json!({
                    "assignment_id": assignment.assignment_id,
                    "holder_actor": assignment.holder_actor,
                    "holder_session_id": assignment.holder_session_id,
                    "lease_until_ts": assignment.lease_until_ts,
                    "clock_op_id": assignment.clock_op_id,
                    "disposition": state.role_assignment_disposition(assignment, &as_of_ts),
                })
            })
            .collect::<Vec<_>>();
        let nearest_active_deadline_ts = state
            .role_active_assignments(&role.role_id, &as_of_ts)
            .into_iter()
            .map(|assignment| assignment.lease_until_ts.as_str())
            .min();
        findings.push(finding(
            "role_coverage_shortfall",
            AuditSeverity::Warning,
            "role",
            &role.role_id,
            format!(
                "role {} has {} active holder(s) for demand {}, a shortfall of {}",
                role.name,
                coverage.active_count,
                coverage.demanded_count,
                coverage.coverage_shortfall
            ),
            json!({
                "role_name": role.name,
                "definition_op_id": role.definition_op_id,
                "assignment_authorities": role.assignment_authorities,
                "coverage": coverage,
                "assignments": assignments,
                "nearest_active_deadline_ts": nearest_active_deadline_ts,
                "observed_at": as_of_ts,
            }),
            "role_assignment_authority",
            "a named assignment authority must grant a bounded assignment to an explicit live holder session; audit will not infer or assign a holder",
        ));
    }

    for candidate in state
        .candidates
        .values()
        .filter(|candidate| candidate.phase == CandidatePhase::Pending)
    {
        let landability = state.candidate_landability_at(&candidate.candidate_id, None, &as_of_ts);
        if !landability.reasons.is_empty() {
            findings.push(finding(
                "candidate_blockers",
                AuditSeverity::Info,
                "candidate",
                &candidate.candidate_id,
                format!(
                    "candidate has {} structured landability blocker(s)",
                    landability.reasons.len()
                ),
                json!({
                    "reason_codes": landability.reason_codes,
                    "reasons": landability.reasons,
                }),
                "candidate_protocol",
                "follow the complete structured blocker list; display truncation is not policy",
            ));
        }
        for reason in landability.reasons.iter().filter(|reason| {
            reason.code == "git_evidence_stale"
                && reason
                    .detail
                    .contains("absent from the declared producer snapshot")
        }) {
            let missing_proposal_op = reason
                .subject
                .as_ref()
                .and_then(|candidate_id| state.candidates.get(candidate_id))
                .map(|other| other.proposal_op_id.clone());
            let uncommitted_in_target = missing_proposal_op
                .as_ref()
                .is_some_and(|op_id| git_backing.uncommitted_op_ids.contains(op_id));
            findings.push(finding(
                "candidate_pair_snapshot_gap",
                AuditSeverity::Error,
                "candidate",
                &candidate.candidate_id,
                reason.detail.clone(),
                json!({
                    "missing_candidate_id": reason.subject,
                    "missing_proposal_op_id": missing_proposal_op,
                    "uncommitted_in_target": uncommitted_in_target,
                    "target_uncommitted_op_count": git_backing.uncommitted_op_count,
                    "target_oldest_uncommitted_age_s": git_backing.oldest_uncommitted_age_s,
                    "observed_at": as_of_ts,
                }),
                "store_sync",
                "synchronize the missing proposal op to a producer, then publish one exact pair observation",
            ));
        }
    }

    for candidate in state.candidates.values() {
        for evidence in candidate.evidence.values() {
            let Some(override_basis) = evidence.payload.git_ancestry_override() else {
                continue;
            };
            findings.push(finding(
                "candidate_ancestry_operator_override_recorded",
                AuditSeverity::Warning,
                "candidate",
                &candidate.candidate_id,
                format!(
                    "operator {} refreshed immutable-producer Git ancestry under an explicit recovery basis",
                    evidence.producer
                ),
                json!({
                    "actor": evidence.producer,
                    "expected_phase_op_id": override_basis.expect_phase_op_id,
                    "expected_authorizer": override_basis.expect_authorizer,
                    "expected_landing_repository_id": override_basis.expect_landing_repository_id,
                    "expected_producers": override_basis.expect_producers,
                    "expected_producer_evidence_op_ids": override_basis.expect_producer_evidence_op_ids,
                    "reason": override_basis.reason,
                    "authority_refs": override_basis.authority_refs,
                    "evidence_id": evidence.evidence_id,
                    "evidence_op_id": evidence.op_id,
                    "recorded_at": evidence.ts,
                }),
                "candidate_governance",
                "review the durable authority references and producer-unavailability basis; this refresh changes only reproducible Git ancestry bookkeeping",
            ));
        }
    }

    for candidate in state.candidates.values() {
        let Some(supersession) = &candidate.supersession else {
            continue;
        };
        if supersession.authority != CandidateSupersessionAuthority::SuccessorAuthorizerContainment
        {
            continue;
        }
        findings.push(finding(
            "candidate_containment_recovery_recorded",
            AuditSeverity::Info,
            "candidate",
            &candidate.candidate_id,
            format!(
                "successor authorizer {} retired the pending predecessor using {} exact containment evidence operation(s)",
                supersession.actor,
                supersession.containment_evidence_op_ids.len()
            ),
            json!({
                "successor_id": supersession.successor_id,
                "actor": supersession.actor,
                "authority": supersession.authority,
                "containment_evidence_op_ids": supersession.containment_evidence_op_ids,
                "supersession_op_id": supersession.op_id,
                "recorded_at": supersession.ts,
            }),
            "candidate_governance",
            "retain the immutable recovery basis for review; containment recovery is supersession, not proof of governed landing",
        ));
    }

    for candidate in state.candidates.values() {
        let Some(reconciliation) = &candidate.reconciled else {
            continue;
        };
        findings.push(finding(
            "candidate_out_of_band_landing_recorded",
            AuditSeverity::Warning,
            "candidate",
            &candidate.candidate_id,
            format!(
                "candidate was observed reachable from {} and recorded as landed outside the formal candidate transition",
                reconciliation.target_ref
            ),
            json!({
                "phase_recorded": candidate.phase,
                "actor": reconciliation.actor,
                "authority": reconciliation.authority,
                "override_basis": reconciliation.override_basis,
                "repository_bridge": reconciliation.repository_bridge,
                "reachability_evidence_id": reconciliation.evidence_id,
                "target_ref": reconciliation.target_ref,
                "target_oid": reconciliation.target_oid,
                "pre_transition_landable": reconciliation.policy_snapshot.pre_transition_landability.landable,
                "pre_transition_reason_codes": reconciliation.policy_snapshot.pre_transition_landability.reason_codes,
                "pre_transition_reasons": reconciliation.policy_snapshot.pre_transition_landability.reasons,
                "authorization_op_id": reconciliation.policy_snapshot.authorization_op_id,
                "authorization_status": reconciliation.policy_snapshot.authorization_status,
                "policy_snapshot": reconciliation.policy_snapshot,
                "reconciliation_op_id": reconciliation.op_id,
                "recorded_at": reconciliation.ts,
            }),
            "candidate_governance",
            "retain this as an explicit reconciliation record; it resolves reachability but does not prove governed review, authorization, or landing",
        ));
        if let Some(override_basis) = &reconciliation.override_basis {
            findings.push(finding(
                "candidate_reconciliation_operator_override_recorded",
                AuditSeverity::Warning,
                "candidate",
                &candidate.candidate_id,
                format!(
                    "operator {} reconciled the candidate by explicit override while preserving original proposal authorizer {}",
                    reconciliation.actor, override_basis.expect_authorizer
                ),
                json!({
                    "actor": reconciliation.actor,
                    "authority": reconciliation.authority,
                    "expected_authorizer": override_basis.expect_authorizer,
                    "expected_landing_repository_id": override_basis.expect_landing_repository_id,
                    "reason": override_basis.reason,
                    "authority_refs": override_basis.authority_refs,
                    "repository_bridge": reconciliation.repository_bridge,
                    "reachability_evidence_id": reconciliation.evidence_id,
                    "target_ref": reconciliation.target_ref,
                    "target_oid": reconciliation.target_oid,
                    "policy_snapshot": reconciliation.policy_snapshot,
                    "reconciliation_op_id": reconciliation.op_id,
                    "recorded_at": reconciliation.ts,
                }),
                "candidate_governance",
                "review the recorded override basis independently; this is explicit bookkeeping recovery, not inherited proposal-authorizer authority",
            ));
        }
        if let Some(bridge) = &reconciliation.repository_bridge {
            findings.push(finding(
                "candidate_reconciliation_repository_bridge_recorded",
                AuditSeverity::Warning,
                "candidate",
                &candidate.candidate_id,
                format!(
                    "candidate was reconciled in repository {} while preserving landing repository {}",
                    bridge.object_availability.repository_id,
                    candidate.landing_repository_id
                ),
                json!({
                    "actor": reconciliation.actor,
                    "preserved_landing_repository_id": candidate.landing_repository_id,
                    "preserved_landing_repository_op_id": bridge.expect_landing_repository_op_id,
                    "observed_repository_id": bridge.object_availability.repository_id,
                    "candidate_oid": bridge.object_availability.candidate_oid,
                    "observed_parent_oids": bridge.object_availability.observed_parent_oids,
                    "git_version": bridge.object_availability.git_version,
                    "reconciliation_op_id": reconciliation.op_id,
                    "recorded_at": reconciliation.ts,
                }),
                "candidate_governance",
                "verify the exact object and parent anchors plus the recorded operator authority; the candidate's repository provenance was not rewritten",
            ));
        }
    }

    let mut target_oid = None;
    let mut repository_id = None;
    if let Some(target_ref) = target_ref {
        let identity = crate::candidate::repository_identity(repo_cwd);
        let target = crate::candidate::resolve_commit(repo_cwd, target_ref);
        if let Ok((observed_repository, _, _)) = &identity {
            repository_id = Some(observed_repository.clone());
        }
        match (identity, target) {
            (Ok((observed_repository, _, _)), Ok(observed_target)) => {
                target_oid = Some(observed_target.clone());
                for candidate in state
                    .candidates
                    .values()
                    .filter(|candidate| candidate.phase == CandidatePhase::Pending)
                {
                    if candidate.landing_repository_id != observed_repository {
                        findings.push(finding(
                            "candidate_landing_repository_mismatch",
                            AuditSeverity::Warning,
                            "candidate",
                            &candidate.candidate_id,
                            "audit repository does not match the candidate's bound landing repository",
                            json!({
                                "proposal_repository_id": candidate.repository_id,
                                "expected_landing_repository_id": candidate.landing_repository_id,
                                "observed_repository_id": observed_repository,
                                "landing_repository_op_id": candidate.landing_repository_op_id,
                                "observed_at": as_of_ts,
                            }),
                            "git_visibility",
                            "run from the repository backing the shared store, or have the candidate authorizer record an explicit landing-repository binding",
                        ));
                        skipped += 1;
                        continue;
                    }
                    match crate::candidate::probe_object_availability(
                        repo_cwd,
                        &candidate.landing_repository_id,
                        &candidate.object_format,
                        &candidate.commit_oid,
                        &candidate.parent_oids,
                    ) {
                        Ok(receipt) if receipt.object_available == Some(false) => {
                            findings.push(finding(
                                "candidate_object_unreachable",
                                AuditSeverity::Warning,
                                "candidate",
                                &candidate.candidate_id,
                                format!(
                                    "candidate commit {} is not readable from the bound landing repository",
                                    candidate.commit_oid
                                ),
                                json!({
                                    "candidate_oid": candidate.commit_oid,
                                    "landing_repository_id": candidate.landing_repository_id,
                                    "object_source": candidate.object_source,
                                    "detail": receipt.detail,
                                    "observed_at": as_of_ts,
                                }),
                                "git_visibility",
                                "transfer or fetch the exact object from the recorded source, then publish candidate object-availability evidence",
                            ));
                            continue;
                        }
                        Ok(receipt) if receipt.object_available == Some(true) => {}
                        Ok(receipt) => {
                            findings.push(finding(
                                "candidate_git_unavailable",
                                AuditSeverity::Warning,
                                "candidate",
                                &candidate.candidate_id,
                                "Git could not determine candidate object availability",
                                json!({
                                    "candidate_oid": candidate.commit_oid,
                                    "landing_repository_id": candidate.landing_repository_id,
                                    "detail": receipt.detail,
                                    "observed_at": as_of_ts,
                                }),
                                "git_visibility",
                                "repair access to the bound landing repository and refresh object availability",
                            ));
                            continue;
                        }
                        Err(error) => {
                            findings.push(finding(
                                "candidate_git_unavailable",
                                AuditSeverity::Warning,
                                "candidate",
                                &candidate.candidate_id,
                                error.clone(),
                                json!({
                                    "candidate_oid": candidate.commit_oid,
                                    "landing_repository_id": candidate.landing_repository_id,
                                    "detail": error,
                                    "observed_at": as_of_ts,
                                }),
                                "git_visibility",
                                "repair access to the bound landing repository and refresh object availability",
                            ));
                            continue;
                        }
                    }
                    match crate::candidate::ancestor_relation(
                        repo_cwd,
                        &candidate.commit_oid,
                        &observed_target,
                    ) {
                        GitRelationKind::Ancestor => findings.push(finding(
                            "candidate_reachable_unrecorded",
                            AuditSeverity::Warning,
                            "candidate",
                            &candidate.candidate_id,
                            format!(
                                "pending candidate commit {} is reachable from observed target {target_ref} at {observed_target}",
                                candidate.commit_oid
                            ),
                            json!({
                                "phase_recorded": candidate.phase,
                                "candidate_oid": candidate.commit_oid,
                                "target_ref": target_ref,
                                "target_oid": observed_target,
                                "observed_at": as_of_ts,
                            }),
                            "candidate_reconciliation",
                            "record an explicitly authorized out-of-band reachability transition; do not claim governed landing",
                        )),
                        GitRelationKind::NotAncestor => {}
                        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => {
                            findings.push(finding(
                                "candidate_git_unavailable",
                                AuditSeverity::Warning,
                                "candidate",
                                &candidate.candidate_id,
                                format!(
                                    "Git could not determine whether {} is an ancestor of {observed_target}",
                                    candidate.commit_oid
                                ),
                                json!({
                                    "candidate_oid": candidate.commit_oid,
                                    "target_ref": target_ref,
                                    "target_oid": observed_target,
                                    "observed_at": as_of_ts,
                                }),
                                "git_visibility",
                                "repair object visibility or shallow history; never infer reachability",
                            ));
                        }
                    }
                }
            }
            (identity, target) => {
                let detail = format!(
                    "repository identity: {}; target resolution: {}",
                    identity
                        .as_ref()
                        .map(|_| "available".to_string())
                        .unwrap_or_else(|error| error.clone()),
                    target
                        .as_ref()
                        .map(|_| "available".to_string())
                        .unwrap_or_else(|error| error.clone())
                );
                findings.push(finding(
                    "candidate_git_unavailable",
                    AuditSeverity::Warning,
                    "repository",
                    target_ref,
                    detail.clone(),
                    json!({
                        "target_ref": target_ref,
                        "detail": detail,
                        "observed_at": as_of_ts,
                    }),
                    "git_visibility",
                    "run the audit from the candidate object database with a resolvable explicit target ref",
                ));
            }
        }
    } else {
        skipped += state
            .candidates
            .values()
            .filter(|candidate| candidate.phase == CandidatePhase::Pending)
            .count();
    }

    let now = &as_of_ts;
    for bead in state.beads.values() {
        let claim_disposition = state.claim_disposition(bead, now);
        if claim_disposition == LeaseDisposition::Orphaned {
            let claim = bead
                .claim
                .as_ref()
                .expect("orphaned claim has a claim record");
            findings.push(finding(
                "orphaned_claim",
                AuditSeverity::Warning,
                "claim",
                &bead.id,
                format!(
                    "claim by {} remains leased to terminal work until {}",
                    claim.claimed_by, claim.lease_until_ts
                ),
                json!({"actor": claim.claimed_by, "lease_until_ts": claim.lease_until_ts}),
                "claim_holder",
                "the holder may release it, or it may expire",
            ));
        }
        if bead.is_deleted() {
            continue;
        }
        let children = state.relation_children_of(&bead.id);
        if bead.status != Status::Closed
            && !children.is_empty()
            && children
                .iter()
                .all(|(child, _)| child.status == Status::Closed)
        {
            findings.push(finding(
                "parent_all_children_closed",
                AuditSeverity::Info,
                "issue",
                &bead.id,
                format!(
                    "recorded parent remains {} while all {} live relation children are closed",
                    bead.status.as_str(),
                    children.len()
                ),
                json!({
                    "status_recorded": bead.status,
                    "closed_children": children.iter().map(|(child, _)| &child.id).collect::<Vec<_>>(),
                }),
                "issue_review",
                "review the parent gate explicitly; audit never closes it",
            ));
        }
        for (parent_id, kind) in &bead.deps {
            if state
                .beads
                .get(parent_id)
                .is_none_or(|parent| parent.is_deleted())
            {
                findings.push(finding(
                    "dangling_dependency",
                    AuditSeverity::Warning,
                    "issue",
                    &bead.id,
                    format!("dependency {kind} points to missing or deleted issue {parent_id}"),
                    json!({"parent_id": parent_id, "kind": kind}),
                    "issue_graph",
                    "repair the explicit dependency edge or restore its target",
                ));
            }
        }
        for (parent_id, kind) in &bead.rels {
            if state
                .beads
                .get(parent_id)
                .is_none_or(|parent| parent.is_deleted())
            {
                findings.push(finding(
                    "dangling_relation",
                    AuditSeverity::Info,
                    "issue",
                    &bead.id,
                    format!("relation {kind} points to missing or deleted issue {parent_id}"),
                    json!({"parent_id": parent_id, "kind": kind}),
                    "issue_graph",
                    "review the organizational relation; audit never rewrites it",
                ));
            }
        }
        if bead.status == Status::Doing && claim_disposition != LeaseDisposition::Active {
            findings.push(finding(
                "doing_without_live_claim",
                AuditSeverity::Info,
                "issue",
                &bead.id,
                "recorded status is doing but no claim lease is active",
                json!({"status_recorded": bead.status, "claim_disposition": claim_disposition}),
                "issue_review",
                "review status and coordination explicitly; this is not proof of abandonment",
            ));
        }
        if bead.assignee.as_deref().is_none_or(str::is_empty) {
            let mut actors = BTreeSet::new();
            if claim_disposition == LeaseDisposition::Active {
                if let Some(claim) = &bead.claim {
                    actors.insert(claim.claimed_by.clone());
                }
            }
            actors.extend(
                state
                    .reservations
                    .values()
                    .filter(|reservation| {
                        reservation.entity == bead.id
                            && state.reservation_disposition(reservation, now)
                                == LeaseDisposition::Active
                    })
                    .map(|reservation| reservation.actor.clone()),
            );
            if !actors.is_empty() {
                findings.push(finding(
                    "unassigned_with_live_coordination",
                    AuditSeverity::Info,
                    "issue",
                    &bead.id,
                    "recorded assignee is empty while named actors hold live coordination leases",
                    json!({"assignee_recorded": null, "coordination_actors": actors}),
                    "issue_review",
                    "display both facts; never infer assignee from claims or reservations",
                ));
            }
        }
    }

    for reservation in state.reservations.values().filter(|reservation| {
        state.reservation_disposition(reservation, now) == LeaseDisposition::Orphaned
    }) {
        findings.push(finding(
            "orphaned_reservation",
            AuditSeverity::Warning,
            "reservation",
            &reservation.reservation_id,
            format!(
                "reservation remains path-blocking on terminal binding {} until {}",
                reservation.entity, reservation.lease_until_ts
            ),
            json!({
                "actor": reservation.actor,
                "entity": reservation.entity,
                "paths": reservation.live_paths(),
                "lease_until_ts": reservation.lease_until_ts,
            }),
            "reservation_holder",
            "an authorized actor may adopt or release it, or wait for expiry",
        ));
    }

    if let Some(stale_after_s) = stale_after_s {
        let statuses = actor_status::actor_statuses(state, None, as_of, stale_after_s);
        for status in statuses
            .iter()
            .filter(|status| status.presence.state == "live" && !status.activity.recent)
        {
            findings.push(finding(
                "live_but_idle",
                AuditSeverity::Info,
                "actor",
                &status.actor,
                format!(
                    "session lease is live but no work or interaction was observed within {stale_after_s}s"
                ),
                json!({
                    "presence": status.presence,
                    "activity": status.activity,
                    "threshold_s": stale_after_s,
                }),
                "actor_review",
                "display lease liveness and activity separately; do not alter presence or reviewer eligibility",
            ));
        }
        let cutoff = as_of
            .checked_sub(jiff::SignedDuration::from_secs(stale_after_s.into()))
            .map(ids::format_rfc3339)
            .unwrap_or_default();
        for request in state.messages.values().filter(|message| {
            message.request_state == Some(RequestState::Open) && message.sent_ts < cutoff
        }) {
            findings.push(finding(
                "aged_open_request",
                AuditSeverity::Warning,
                "request",
                &request.msg_id,
                format!(
                    "request from {} to {} remains explicitly open past the {}s threshold",
                    request.from, request.to, stale_after_s
                ),
                json!({
                    "from": request.from,
                    "to": request.to,
                    "sent_ts": request.sent_ts,
                    "threshold_s": stale_after_s,
                }),
                "request_owner",
                "review the request explicitly; acknowledgement alone is not resolution",
            ));
        }
    }

    findings.sort_by(|left, right| {
        left.severity
            .rank()
            .cmp(&right.severity.rank())
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.subject.cmp(&right.subject))
            .then_with(|| left.message.cmp(&right.message))
    });
    let mut summary = AuditSummary {
        skipped,
        ..AuditSummary::default()
    };
    for finding in &findings {
        match finding.severity {
            AuditSeverity::Error => summary.error += 1,
            AuditSeverity::Warning => summary.warning += 1,
            AuditSeverity::Info => summary.info += 1,
        }
    }
    let ok = !findings
        .iter()
        .any(|finding| fail_on.triggered_by(finding.severity));

    Ok(AuditReport {
        schema_version: AUDIT_SCHEMA_VERSION,
        ok,
        as_of_ts,
        fail_on: fail_on.as_str().into(),
        context: AuditContext {
            store_id: format.store_id,
            target_ref: target_ref.map(str::to_string),
            target_oid,
            repository_id,
        },
        git_backing,
        summary,
        findings,
    })
}
