//! Candidate protocol value types and Git evidence probes.
//!
//! Reducer replay consumes only serialized receipts. Git is consulted here by
//! explicit CLI operations before publication, never while materializing state.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::errors::{MoteError, MoteResult};

pub const CANDIDATE_PROTOCOL_VERSION: u32 = 1;
pub const CANDIDATE_ROLE_REVIEW_VERSION: u32 = 2;
pub const CANDIDATE_PORTABLE_PROTOCOL_VERSION: u32 = 3;
pub const GIT_RELATION_SCHEMA_V2: u32 = 2;
pub const GIT_ANCESTRY_EVIDENCE: &str = "git-ancestry";
pub const GIT_LANDING_EVIDENCE: &str = "git-landing";
pub const GIT_REACHABILITY_EVIDENCE: &str = "git-reachability";
pub const GIT_OBJECT_AVAILABILITY_EVIDENCE: &str = "git-object-availability";

fn legacy_git_relation_schema() -> u32 {
    1
}

fn is_legacy_git_relation_schema(value: &u32) -> bool {
    *value == legacy_git_relation_schema()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidatePhase {
    Pending,
    Superseded,
    Abandoned,
    Landed,
    LandedOutOfBand,
}

impl CandidatePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Superseded => "superseded",
            Self::Abandoned => "abandoned",
            Self::Landed => "landed",
            Self::LandedOutOfBand => "landed_out_of_band",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateReconciliationAuthority {
    ProposalAuthorizer,
}

impl CandidateReconciliationAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProposalAuthorizer => "proposal_authorizer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSupersessionAuthority {
    PredecessorProposer,
    PredecessorAuthorizer,
    SuccessorAuthorizerContainment,
}

impl CandidateSupersessionAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PredecessorProposer => "predecessor_proposer",
            Self::PredecessorAuthorizer => "predecessor_authorizer",
            Self::SuccessorAuthorizerContainment => "successor_authorizer_containment",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSupersedeRecovery {
    pub authority: CandidateSupersessionAuthority,
    pub expect_successor_phase: String,
    pub containment_evidence_op_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateContainmentBasis {
    pub predecessor_candidate_id: String,
    pub successor_candidate_id: String,
    pub predecessor_to_successor_base: GitRelationKind,
    pub predecessor_to_successor_tip: GitRelationKind,
    pub evidence_op_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    Block,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleReviewRequirement {
    pub role_id: String,
    pub definition_op_id: String,
    pub required_approvals: u32,
}

/// Version-2 review policy. It is carried in a separate field while the
/// legacy `reviewers` field is empty so binaries that do not understand role
/// review policy reject the proposal instead of silently weakening it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReviewPolicy {
    pub named_reviewers: Vec<String>,
    pub role_requirements: Vec<RoleReviewRequirement>,
}

/// Provenance for the object database and ref from which a proposal was
/// resolved. `locator` is explicit user-supplied coordination metadata; Mote
/// never fetches it implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateObjectSource {
    pub repository_id: String,
    pub commit_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locator: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRoleReviewBinding {
    pub role_id: String,
    pub assignment_id: String,
    pub expect_assignment: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateReviewQualification {
    NamedReviewer,
    RoleAssignment {
        role_id: String,
        assignment_id: String,
        assignment_clock_op_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamedReviewStatus {
    pub reviewer: String,
    pub verdict: Option<ReviewVerdict>,
    pub review_op_id: Option<String>,
    pub satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleReviewEntryStatus {
    pub reviewer: String,
    pub assignment_id: String,
    pub verdict: ReviewVerdict,
    pub review_op_id: String,
    pub eligible: bool,
    pub reason_code: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleReviewRequirementStatus {
    pub role_id: String,
    pub role_name: Option<String>,
    pub definition_op_id: String,
    pub required_approvals: u32,
    pub eligible_approval_count: u32,
    pub eligible_block_count: u32,
    pub satisfied: bool,
    pub reviews: Vec<RoleReviewEntryStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateReviewStatus {
    pub as_of_ts: String,
    pub named: Vec<NamedReviewStatus>,
    pub roles: Vec<RoleReviewRequirementStatus>,
    pub satisfied: bool,
}

impl ReviewVerdict {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "approve" => Some(Self::Approve),
            "block" => Some(Self::Block),
            "comment" => Some(Self::Comment),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Block => "block",
            Self::Comment => "comment",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    Pass,
    Fail,
    Unavailable,
    Ambiguous,
}

impl EvidenceOutcome {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "unavailable" => Some(Self::Unavailable),
            "ambiguous" => Some(Self::Ambiguous),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unavailable => "unavailable",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationStatus {
    Granted,
    Conditional,
    Revoked,
    Consumed,
}

impl AuthorizationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Conditional => "conditional",
            Self::Revoked => "revoked",
            Self::Consumed => "consumed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRelationKind {
    Ancestor,
    NotAncestor,
    Unavailable,
    Ambiguous,
}

impl GitRelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ancestor => "ancestor",
            Self::NotAncestor => "not_ancestor",
            Self::Unavailable => "unavailable",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub name: String,
    pub kind: String,
    pub producers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCandidateRelation {
    pub candidate_id: String,
    pub proposal_op_id: String,
    pub commit_oid: String,
    /// Immutable base of the known candidate. Required for schema-v2 pair
    /// observations; absent from legacy rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_oid: Option<String>,
    /// Relation of the known candidate commit to this candidate's immutable base.
    /// Missing on legacy receipts, which consumers must treat as ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_relation: Option<GitRelationKind>,
    /// Relation of the known candidate commit to this candidate's tip.
    pub relation: GitRelationKind,
    /// Relation of this receipt's subject commit to the known candidate's base.
    /// Required to reuse a schema-v2 row in the reciprocal direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_to_known_base: Option<GitRelationKind>,
    /// Relation of this receipt's subject commit to the known candidate's tip.
    /// Required to reuse a schema-v2 row in the reciprocal direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_to_known_tip: Option<GitRelationKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSnapshotProvenance {
    pub store_id: String,
    pub observed_candidates: Vec<(String, String)>,
    pub replayed_op_count: u64,
    pub replayed_op_ids_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_git_head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncommitted_op_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAncestryReceipt {
    #[serde(
        default = "legacy_git_relation_schema",
        skip_serializing_if = "is_legacy_git_relation_schema"
    )]
    pub relation_schema: u32,
    pub repository_id: String,
    pub object_format: String,
    pub common_dir_hash: String,
    pub commit_oid: String,
    pub base_oid: String,
    pub parent_oids: Vec<String>,
    pub base_is_ancestor: Option<bool>,
    pub candidate_relations: Vec<GitCandidateRelation>,
    pub covered_candidates: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_snapshot: Option<Box<CandidateSnapshotProvenance>>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLandingReceipt {
    pub repository_id: String,
    pub object_format: String,
    pub candidate_oid: String,
    pub target_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_tip: Option<String>,
    pub after_tip: String,
    pub candidate_reachable: Option<bool>,
    pub authorization_op_id: String,
    pub basis_op_ids: Vec<String>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Fresh, exact observation that a candidate commit is reachable from one
/// explicitly named target ref. Unlike `GitLandingReceipt`, this receipt is
/// not tied to or evidence of a governed landing authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitReachabilityReceipt {
    pub repository_id: String,
    pub object_format: String,
    pub candidate_oid: String,
    pub target_ref: String,
    pub observed_target_oid: String,
    pub candidate_reachable: Option<bool>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Exact observation that the candidate commit object is readable from the
/// repository currently bound for landing. This is an operational visibility
/// receipt, not evidence that a landing occurred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitObjectAvailabilityReceipt {
    pub repository_id: String,
    pub object_format: String,
    pub candidate_oid: String,
    pub observed_parent_oids: Vec<String>,
    pub object_available: Option<bool>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateEvidencePayload {
    GitAncestry(GitAncestryReceipt),
    GitLanding(GitLandingReceipt),
    GitReachability(GitReachabilityReceipt),
    GitObjectAvailability(GitObjectAvailabilityReceipt),
    External {
        digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownCandidate {
    pub candidate_id: String,
    pub proposal_op_id: String,
    pub repository_id: String,
    pub landing_repository_id: String,
    pub object_format: String,
    pub commit_oid: String,
    pub base_oid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandabilityReasonClass {
    Substantive,
    Process,
    Bookkeeping,
}

impl LandabilityReasonClass {
    pub const ALL: [Self; 3] = [Self::Substantive, Self::Process, Self::Bookkeeping];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Substantive => "substantive",
            Self::Process => "process",
            Self::Bookkeeping => "bookkeeping",
        }
    }
}

/// Closed vocabulary for reducer-derived landability reasons.
///
/// JSON retains the historical string `code`; this enum makes new reducer
/// reasons choose a class and blocking policy at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LandabilityReasonCode {
    CandidateMissing,
    PhaseNotPending,
    ReviewBlocking,
    ReviewMissing,
    ReviewRoleUnavailable,
    ReviewRoleBlocking,
    ReviewRoleApprovalIneligible,
    ReviewRoleQuorumMissing,
    EvidenceUnavailable,
    EvidenceFailed,
    EvidenceMissing,
    BaseNotAncestor,
    GitEvidenceStale,
    AncestorAmbiguous,
    AncestorMissing,
    RepositoryMismatch,
    ProposalAnchorMismatch,
    GitEvidenceUnavailable,
    GitEvidenceMissing,
    ObjectUnreachable,
    ObjectAvailabilityUnavailable,
    ObjectAvailabilityMissing,
    ActorNotGrantee,
    ConditionUnsatisfied,
    AuthorizationRevoked,
    AuthorizationAbsent,
    SupersessionCycle,
    SupersessionBroken,
    AncestorSupersessionUnresolved,
    AncestorBlocked,
    AncestorAuthorizationRevoked,
    AncestorPending,
    AncestorAbandoned,
}

impl LandabilityReasonCode {
    pub const ALL: [Self; 33] = [
        Self::CandidateMissing,
        Self::PhaseNotPending,
        Self::ReviewBlocking,
        Self::ReviewMissing,
        Self::ReviewRoleUnavailable,
        Self::ReviewRoleBlocking,
        Self::ReviewRoleApprovalIneligible,
        Self::ReviewRoleQuorumMissing,
        Self::EvidenceUnavailable,
        Self::EvidenceFailed,
        Self::EvidenceMissing,
        Self::BaseNotAncestor,
        Self::GitEvidenceStale,
        Self::AncestorAmbiguous,
        Self::AncestorMissing,
        Self::RepositoryMismatch,
        Self::ProposalAnchorMismatch,
        Self::GitEvidenceUnavailable,
        Self::GitEvidenceMissing,
        Self::ObjectUnreachable,
        Self::ObjectAvailabilityUnavailable,
        Self::ObjectAvailabilityMissing,
        Self::ActorNotGrantee,
        Self::ConditionUnsatisfied,
        Self::AuthorizationRevoked,
        Self::AuthorizationAbsent,
        Self::SupersessionCycle,
        Self::SupersessionBroken,
        Self::AncestorSupersessionUnresolved,
        Self::AncestorBlocked,
        Self::AncestorAuthorizationRevoked,
        Self::AncestorPending,
        Self::AncestorAbandoned,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CandidateMissing => "candidate_missing",
            Self::PhaseNotPending => "phase_not_pending",
            Self::ReviewBlocking => "review_blocking",
            Self::ReviewMissing => "review_missing",
            Self::ReviewRoleUnavailable => "review_role_unavailable",
            Self::ReviewRoleBlocking => "review_role_blocking",
            Self::ReviewRoleApprovalIneligible => "review_role_approval_ineligible",
            Self::ReviewRoleQuorumMissing => "review_role_quorum_missing",
            Self::EvidenceUnavailable => "evidence_unavailable",
            Self::EvidenceFailed => "evidence_failed",
            Self::EvidenceMissing => "evidence_missing",
            Self::BaseNotAncestor => "base_not_ancestor",
            Self::GitEvidenceStale => "git_evidence_stale",
            Self::AncestorAmbiguous => "ancestor_ambiguous",
            Self::AncestorMissing => "ancestor_missing",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::ProposalAnchorMismatch => "proposal_anchor_mismatch",
            Self::GitEvidenceUnavailable => "git_evidence_unavailable",
            Self::GitEvidenceMissing => "git_evidence_missing",
            Self::ObjectUnreachable => "object_unreachable",
            Self::ObjectAvailabilityUnavailable => "object_availability_unavailable",
            Self::ObjectAvailabilityMissing => "object_availability_missing",
            Self::ActorNotGrantee => "actor_not_grantee",
            Self::ConditionUnsatisfied => "condition_unsatisfied",
            Self::AuthorizationRevoked => "authorization_revoked",
            Self::AuthorizationAbsent => "authorization_absent",
            Self::SupersessionCycle => "supersession_cycle",
            Self::SupersessionBroken => "supersession_broken",
            Self::AncestorSupersessionUnresolved => "ancestor_supersession_unresolved",
            Self::AncestorBlocked => "ancestor_blocked",
            Self::AncestorAuthorizationRevoked => "ancestor_authorization_revoked",
            Self::AncestorPending => "ancestor_pending",
            Self::AncestorAbandoned => "ancestor_abandoned",
        }
    }

    pub const fn class(self) -> LandabilityReasonClass {
        match self {
            Self::ReviewBlocking
            | Self::ReviewRoleBlocking
            | Self::EvidenceFailed
            | Self::AncestorBlocked => LandabilityReasonClass::Substantive,
            Self::ReviewMissing
            | Self::ReviewRoleUnavailable
            | Self::ReviewRoleApprovalIneligible
            | Self::ReviewRoleQuorumMissing
            | Self::EvidenceUnavailable
            | Self::EvidenceMissing
            | Self::GitEvidenceUnavailable
            | Self::GitEvidenceMissing
            | Self::ObjectUnreachable
            | Self::ObjectAvailabilityUnavailable
            | Self::ObjectAvailabilityMissing
            | Self::ActorNotGrantee
            | Self::ConditionUnsatisfied
            | Self::AuthorizationRevoked
            | Self::AuthorizationAbsent
            | Self::AncestorAuthorizationRevoked => LandabilityReasonClass::Process,
            Self::CandidateMissing
            | Self::PhaseNotPending
            | Self::BaseNotAncestor
            | Self::GitEvidenceStale
            | Self::AncestorAmbiguous
            | Self::AncestorMissing
            | Self::RepositoryMismatch
            | Self::ProposalAnchorMismatch
            | Self::SupersessionCycle
            | Self::SupersessionBroken
            | Self::AncestorSupersessionUnresolved
            | Self::AncestorPending
            | Self::AncestorAbandoned => LandabilityReasonClass::Bookkeeping,
        }
    }

    /// Classification is independent from policy. All current reasons retain
    /// their historical fail-closed behavior.
    pub const fn blocking(self) -> bool {
        true
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LandabilityReason {
    pub code: String,
    pub class: LandabilityReasonClass,
    pub blocking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub detail: String,
}

impl LandabilityReason {
    pub fn new(
        code: LandabilityReasonCode,
        subject: Option<impl Into<String>>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            code: code.as_str().to_string(),
            class: code.class(),
            blocking: code.blocking(),
            subject: subject.map(Into::into),
            detail: detail.into(),
        }
    }
}

impl<'de> Deserialize<'de> for LandabilityReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireReason {
            code: String,
            #[serde(default)]
            class: Option<LandabilityReasonClass>,
            #[serde(default)]
            blocking: Option<bool>,
            #[serde(default)]
            subject: Option<String>,
            detail: String,
        }

        let wire = WireReason::deserialize(deserializer)?;
        let known = LandabilityReasonCode::parse(&wire.code);
        Ok(Self {
            code: wire.code,
            class: wire
                .class
                .or_else(|| known.map(LandabilityReasonCode::class))
                .unwrap_or(LandabilityReasonClass::Bookkeeping),
            blocking: wire
                .blocking
                .unwrap_or_else(|| known.is_none_or(LandabilityReasonCode::blocking)),
            subject: wire.subject,
            detail: wire.detail,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Landability {
    pub landable: bool,
    pub reason_codes: Vec<String>,
    pub reasons: Vec<LandabilityReason>,
}

/// Exact reducer-derived policy register compared by an out-of-band
/// reconciliation op. It preserves the pre-transition blockers and makes any
/// intervening review, evidence, authorization, or pair-evidence update fail
/// closed instead of silently changing the recorded basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidatePolicySnapshot {
    pub phase_op_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_policy_op_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_repository_op_id: Option<String>,
    pub review_op_ids: Vec<String>,
    pub evidence_op_ids: Vec<String>,
    pub pair_evidence_op_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_op_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_status: Option<AuthorizationStatus>,
    pub pre_transition_landability: Landability,
}

impl Landability {
    pub fn from_reasons(mut reasons: Vec<LandabilityReason>) -> Self {
        reasons.sort_by(|a, b| {
            a.code
                .cmp(&b.code)
                .then_with(|| a.subject.cmp(&b.subject))
                .then_with(|| a.detail.cmp(&b.detail))
        });
        let mut reason_codes: Vec<String> = reasons.iter().map(|r| r.code.clone()).collect();
        reason_codes.sort();
        reason_codes.dedup();
        Self {
            landable: !reasons.iter().any(|reason| reason.blocking),
            reason_codes,
            reasons,
        }
    }
}

pub fn evidence_id<T: Serialize>(value: &T) -> MoteResult<String> {
    let json = serde_json::to_value(value)?;
    Ok(format!(
        "evid-{}",
        blake3::hash(&canonical::encode(&json)).to_hex()
    ))
}

pub fn action_digest<T: Serialize>(value: &T) -> MoteResult<String> {
    let mut json = serde_json::to_value(value)?;
    if let Some(object) = json.as_object_mut() {
        object.remove("v");
        object.remove("op");
        object.remove("ts");
        object.remove("idempotency_key");
    }
    Ok(blake3::hash(&canonical::encode(&json)).to_hex().to_string())
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("git {} failed to start: {error}", args.join(" ")))
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(cwd, args)?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|error| format!("git {} returned non-UTF-8 output: {error}", args.join(" ")))
}

fn canonical_common_dir(cwd: &Path) -> Result<PathBuf, String> {
    let raw = git_text(cwd, &["rev-parse", "--git-common-dir"])?;
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "cannot canonicalize git common dir {}: {error}",
            path.display()
        )
    })
}

pub(crate) fn repository_identity(cwd: &Path) -> Result<(String, String, String), String> {
    let object_format = git_text(cwd, &["rev-parse", "--show-object-format"])?;
    if object_format != "sha1" && object_format != "sha256" {
        return Err(format!("unsupported Git object format {object_format}"));
    }
    let common_dir = canonical_common_dir(cwd)?;
    let common_dir_hash = blake3::hash(common_dir.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    let mut identity = Vec::new();
    identity.extend_from_slice(object_format.as_bytes());
    identity.push(0);
    identity.extend_from_slice(common_dir_hash.as_bytes());
    let repository_id = format!("repo-{}", blake3::hash(&identity).to_hex());
    Ok((repository_id, object_format, common_dir_hash))
}

pub(crate) fn resolve_commit(cwd: &Path, reference: &str) -> Result<String, String> {
    git_text(
        cwd,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
}

fn commit_parents(cwd: &Path, commit: &str) -> Result<Vec<String>, String> {
    let line = git_text(cwd, &["rev-list", "--parents", "-n", "1", commit])?;
    let mut ids = line.split_whitespace();
    let observed = ids
        .next()
        .ok_or_else(|| "git rev-list returned no commit".to_string())?;
    if observed != commit {
        return Err(format!(
            "git rev-list returned {observed}, expected {commit}"
        ));
    }
    Ok(ids.map(str::to_string).collect())
}

pub(crate) fn ancestor_relation(cwd: &Path, ancestor: &str, descendant: &str) -> GitRelationKind {
    match git_output(cwd, &["merge-base", "--is-ancestor", ancestor, descendant]) {
        Ok(output) if output.status.code() == Some(0) => GitRelationKind::Ancestor,
        Ok(output) if output.status.code() == Some(1) => GitRelationKind::NotAncestor,
        Ok(_) => GitRelationKind::Ambiguous,
        Err(_) => GitRelationKind::Unavailable,
    }
}

fn git_version(cwd: &Path) -> String {
    git_text(cwd, &["--version"]).unwrap_or_else(|error| format!("unavailable: {error}"))
}

pub fn probe_ancestry(
    cwd: &Path,
    commit_ref: &str,
    base_ref: &str,
    known: &[KnownCandidate],
) -> Result<GitAncestryReceipt, String> {
    probe_ancestry_in_landing_repository(cwd, commit_ref, base_ref, known, None)
}

pub fn probe_ancestry_for_landing_repository(
    cwd: &Path,
    commit_ref: &str,
    base_ref: &str,
    known: &[KnownCandidate],
    landing_repository_id: &str,
) -> Result<GitAncestryReceipt, String> {
    probe_ancestry_in_landing_repository(
        cwd,
        commit_ref,
        base_ref,
        known,
        Some(landing_repository_id),
    )
}

fn probe_ancestry_in_landing_repository(
    cwd: &Path,
    commit_ref: &str,
    base_ref: &str,
    known: &[KnownCandidate],
    landing_repository_id: Option<&str>,
) -> Result<GitAncestryReceipt, String> {
    let (repository_id, object_format, common_dir_hash) = repository_identity(cwd)?;
    let commit_oid = resolve_commit(cwd, commit_ref)?;
    let base_oid = resolve_commit(cwd, base_ref)?;
    let parent_oids = commit_parents(cwd, &commit_oid)?;
    let base_relation = ancestor_relation(cwd, &base_oid, &commit_oid);
    let base_is_ancestor = match base_relation {
        GitRelationKind::Ancestor => Some(true),
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };

    let mut candidate_relations = Vec::new();
    let mut covered_candidates = Vec::new();
    for candidate in known {
        let same_domain = landing_repository_id.map_or_else(
            || candidate.repository_id == repository_id,
            |landing_repository_id| {
                candidate.landing_repository_id == landing_repository_id
                    && candidate.object_format == object_format
            },
        );
        if !same_domain {
            continue;
        }
        candidate_relations.push(GitCandidateRelation {
            candidate_id: candidate.candidate_id.clone(),
            proposal_op_id: candidate.proposal_op_id.clone(),
            commit_oid: candidate.commit_oid.clone(),
            base_oid: Some(candidate.base_oid.clone()),
            base_relation: Some(ancestor_relation(cwd, &candidate.commit_oid, &base_oid)),
            relation: ancestor_relation(cwd, &candidate.commit_oid, &commit_oid),
            subject_to_known_base: Some(ancestor_relation(cwd, &commit_oid, &candidate.base_oid)),
            subject_to_known_tip: Some(ancestor_relation(cwd, &commit_oid, &candidate.commit_oid)),
        });
        covered_candidates.push((
            candidate.candidate_id.clone(),
            candidate.proposal_op_id.clone(),
        ));
    }
    candidate_relations.sort_by(|a, b| a.candidate_id.cmp(&b.candidate_id));
    covered_candidates.sort();

    Ok(GitAncestryReceipt {
        relation_schema: GIT_RELATION_SCHEMA_V2,
        repository_id,
        object_format,
        common_dir_hash,
        commit_oid,
        base_oid,
        parent_oids,
        base_is_ancestor,
        candidate_relations,
        covered_candidates,
        producer_snapshot: None,
        git_version: git_version(cwd),
        detail: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn probe_landing(
    cwd: &Path,
    repository_id: &str,
    object_format: &str,
    candidate_oid: &str,
    target_ref: &str,
    before_tip: Option<&str>,
    authorization_op_id: &str,
    basis_op_ids: Vec<String>,
) -> Result<GitLandingReceipt, String> {
    let (observed_repository, observed_format, _) = repository_identity(cwd)?;
    if observed_repository != repository_id || observed_format != object_format {
        return Err("landing repository identity does not match candidate".into());
    }
    let after_tip = resolve_commit(cwd, target_ref)?;
    let before_tip = before_tip
        .map(|reference| resolve_commit(cwd, reference))
        .transpose()?;
    let relation = ancestor_relation(cwd, candidate_oid, &after_tip);
    let candidate_reachable = match relation {
        GitRelationKind::Ancestor => Some(true),
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };
    Ok(GitLandingReceipt {
        repository_id: observed_repository,
        object_format: observed_format,
        candidate_oid: candidate_oid.to_string(),
        target_ref: target_ref.to_string(),
        before_tip,
        after_tip,
        candidate_reachable,
        authorization_op_id: authorization_op_id.to_string(),
        basis_op_ids,
        git_version: git_version(cwd),
        detail: None,
    })
}

pub fn probe_reachability(
    cwd: &Path,
    repository_id: &str,
    object_format: &str,
    candidate_oid: &str,
    target_ref: &str,
) -> Result<GitReachabilityReceipt, String> {
    let (observed_repository, observed_format, _) = repository_identity(cwd)?;
    if observed_repository != repository_id || observed_format != object_format {
        return Err("reachability repository identity does not match candidate".into());
    }
    let observed_target_oid = resolve_commit(cwd, target_ref)?;
    let relation = ancestor_relation(cwd, candidate_oid, &observed_target_oid);
    let candidate_reachable = match relation {
        GitRelationKind::Ancestor => Some(true),
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };
    Ok(GitReachabilityReceipt {
        repository_id: observed_repository,
        object_format: observed_format,
        candidate_oid: candidate_oid.to_string(),
        target_ref: target_ref.to_string(),
        observed_target_oid,
        candidate_reachable,
        git_version: git_version(cwd),
        detail: None,
    })
}

pub fn probe_object_availability(
    cwd: &Path,
    repository_id: &str,
    object_format: &str,
    candidate_oid: &str,
    expected_parent_oids: &[String],
) -> Result<GitObjectAvailabilityReceipt, String> {
    let (observed_repository, observed_format, _) = repository_identity(cwd)?;
    if observed_repository != repository_id || observed_format != object_format {
        return Err(
            "object-availability repository identity does not match candidate landing repository"
                .into(),
        );
    }
    let object_expression = format!("{candidate_oid}^{{commit}}");
    let output = git_output(cwd, &["cat-file", "-e", &object_expression])?;
    let (object_available, observed_parent_oids, detail) = if output.status.success() {
        let observed_oid = resolve_commit(cwd, candidate_oid)?;
        let parents = commit_parents(cwd, &observed_oid)?;
        if observed_oid == candidate_oid && parents == expected_parent_oids {
            (Some(true), parents, None)
        } else {
            (
                Some(false),
                parents,
                Some(
                    "resolved commit does not match the immutable object and parent anchors".into(),
                ),
            )
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        (
            Some(false),
            Vec::new(),
            Some(if stderr.is_empty() {
                format!("git cat-file exited with {}", output.status)
            } else {
                stderr
            }),
        )
    };
    Ok(GitObjectAvailabilityReceipt {
        repository_id: observed_repository,
        object_format: observed_format,
        candidate_oid: candidate_oid.to_string(),
        observed_parent_oids,
        object_available,
        git_version: git_version(cwd),
        detail,
    })
}

pub fn validate_full_oid(object_format: &str, oid: &str) -> bool {
    let expected = match object_format {
        "sha1" => 40,
        "sha256" => 64,
        _ => return false,
    };
    oid.len() == expected
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn git_probe_error(error: String) -> MoteError {
    MoteError::Other(format!("Git evidence unavailable: {error}"))
}

#[cfg(test)]
mod landability_reason_tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::{Landability, LandabilityReason, LandabilityReasonClass, LandabilityReasonCode};

    #[test]
    fn every_reason_code_has_one_stable_class_and_remains_blocking() {
        let names = LandabilityReasonCode::ALL
            .into_iter()
            .map(LandabilityReasonCode::as_str)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), LandabilityReasonCode::ALL.len());
        assert!(
            LandabilityReasonCode::ALL
                .into_iter()
                .all(LandabilityReasonCode::blocking)
        );

        let substantive = [
            LandabilityReasonCode::ReviewBlocking,
            LandabilityReasonCode::ReviewRoleBlocking,
            LandabilityReasonCode::EvidenceFailed,
            LandabilityReasonCode::AncestorBlocked,
        ];
        assert!(
            substantive
                .into_iter()
                .all(|code| { code.class() == LandabilityReasonClass::Substantive })
        );

        let process = [
            LandabilityReasonCode::ReviewMissing,
            LandabilityReasonCode::ReviewRoleUnavailable,
            LandabilityReasonCode::ReviewRoleApprovalIneligible,
            LandabilityReasonCode::ReviewRoleQuorumMissing,
            LandabilityReasonCode::EvidenceUnavailable,
            LandabilityReasonCode::EvidenceMissing,
            LandabilityReasonCode::GitEvidenceUnavailable,
            LandabilityReasonCode::GitEvidenceMissing,
            LandabilityReasonCode::ObjectUnreachable,
            LandabilityReasonCode::ObjectAvailabilityUnavailable,
            LandabilityReasonCode::ObjectAvailabilityMissing,
            LandabilityReasonCode::ActorNotGrantee,
            LandabilityReasonCode::ConditionUnsatisfied,
            LandabilityReasonCode::AuthorizationRevoked,
            LandabilityReasonCode::AuthorizationAbsent,
            LandabilityReasonCode::AncestorAuthorizationRevoked,
        ];
        assert!(
            process
                .into_iter()
                .all(|code| code.class() == LandabilityReasonClass::Process)
        );

        let classified = LandabilityReasonCode::ALL
            .into_iter()
            .filter(|code| {
                substantive.contains(code)
                    || process.contains(code)
                    || code.class() == LandabilityReasonClass::Bookkeeping
            })
            .count();
        assert_eq!(classified, LandabilityReasonCode::ALL.len());
    }

    #[test]
    fn legacy_reason_json_derives_additive_fields_from_its_code() {
        let legacy = json!({
            "code": "review_blocking",
            "subject": "reviewer",
            "detail": "latest verdict is block"
        });
        let reason: LandabilityReason = serde_json::from_value(legacy).unwrap();
        assert_eq!(reason.class, LandabilityReasonClass::Substantive);
        assert!(reason.blocking);
        let current = serde_json::to_value(reason).unwrap();
        assert_eq!(current["class"], "substantive");
        assert_eq!(current["blocking"], true);
    }

    #[test]
    fn blocking_is_independent_from_class_and_reason_presence() {
        let informational = LandabilityReason {
            code: "future_informational_reason".into(),
            class: LandabilityReasonClass::Bookkeeping,
            blocking: false,
            subject: None,
            detail: "advisory only".into(),
        };
        let landability = Landability::from_reasons(vec![informational]);
        assert!(landability.landable);
        assert_eq!(landability.reason_codes, ["future_informational_reason"]);
        assert_eq!(landability.reasons.len(), 1);
    }
}
