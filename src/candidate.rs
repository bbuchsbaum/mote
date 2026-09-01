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
pub const GIT_RECONCILIATION_REACHABILITY_EVIDENCE: &str = "git-reconciliation-reachability";
pub const GIT_OBJECT_AVAILABILITY_EVIDENCE: &str = "git-object-availability";
pub const GIT_TARGET_SCOPE_EVIDENCE: &str = "git-target-scope";
pub const GIT_TARGET_SCOPE_SCHEMA_V1: u32 = 1;

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
    ExplicitOperatorOverride,
}

impl CandidateReconciliationAuthority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProposalAuthorizer => "proposal_authorizer",
            Self::ExplicitOperatorOverride => "explicit_operator_override",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateOperatorOverride {
    pub expect_authorizer: String,
    pub expect_landing_repository_id: String,
    pub reason: String,
    pub authority_refs: Vec<String>,
}

/// Audited authority used to refresh independently reproducible Git ancestry
/// when every immutable proposal producer is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateEvidenceOperatorOverride {
    pub expect_phase_op_id: String,
    pub expect_authorizer: String,
    pub expect_landing_repository_id: String,
    pub expect_producers: Vec<String>,
    pub expect_producer_evidence_op_ids: Vec<String>,
    pub reason: String,
    pub authority_refs: Vec<String>,
}

/// Exact proof that an out-of-band reconciliation was observed in a different
/// repository without changing the candidate's recorded landing repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReconciliationRepositoryBridge {
    pub expect_landing_repository_op_id: String,
    pub object_availability: GitObjectAvailabilityReceipt,
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

pub fn git_ancestry_producers(requirements: &[EvidenceRequirement]) -> Vec<String> {
    let mut producers = requirements
        .iter()
        .filter(|requirement| {
            requirement.name == GIT_ANCESTRY_EVIDENCE && requirement.kind == "git"
        })
        .flat_map(|requirement| requirement.producers.iter().cloned())
        .collect::<Vec<_>>();
    producers.sort();
    producers.dedup();
    producers
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
    /// Canonical mutable Git ref whose reflog proved the immediate preimage.
    /// Optional only so historical landing receipts remain replayable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_ref_full_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_tip: Option<String>,
    pub after_tip: String,
    /// Tree derived from `after_tip`; optional only for historical replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_tree_oid: Option<String>,
    /// Exact no-rename diff from the proved immediate preimage to `after_tip`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landing_effect_paths: Vec<String>,
    pub candidate_reachable: Option<bool>,
    pub authorization_op_id: String,
    pub basis_op_ids: Vec<String>,
    /// Exact target-scope observation used as the pre-landing target CAS.
    /// Optional only so historical landing receipts remain replayable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_evidence_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope_op_id: Option<String>,
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

/// Exact observation that the candidate commit object and parents are readable
/// from one named repository. Ordinary availability evidence binds that name to
/// the landing repository; a reconciliation bridge records the narrower,
/// explicit cross-repository exception. This is not evidence of a landing.
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

/// Exact, replayable observation of the paths that can affect one named
/// landing target. The CLI derives repository and object identities from Git;
/// callers choose only the ref to observe.
///
/// `effective_paths` is the sorted union of the candidate-side
/// `merge-base..candidate` diffs and the prospective target-to-merge-result
/// diff. Diffs run with rename detection disabled, so candidate renames retain
/// both endpoints while target-side rename mapping contributes the actual
/// destination that the merge would modify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitTargetScopeReceipt {
    pub scope_schema: u32,
    pub repository_id: String,
    pub landing_repository_op_id: String,
    pub object_format: String,
    pub candidate_oid: String,
    pub candidate_base_oid: String,
    pub target_ref: String,
    pub target_ref_full_name: String,
    pub observed_target_oid: String,
    pub candidate_is_ancestor_of_target: Option<bool>,
    pub base_is_ancestor_of_target: Option<bool>,
    pub merge_base_oids: Vec<String>,
    pub prospective_merge_tree_oid: String,
    pub candidate_side_paths: Vec<String>,
    pub prospective_target_effect_paths: Vec<String>,
    pub effective_paths: Vec<String>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateEvidencePayload {
    GitAncestry(GitAncestryReceipt),
    GitAncestryOverride {
        receipt: GitAncestryReceipt,
        override_basis: CandidateEvidenceOperatorOverride,
    },
    GitLanding(GitLandingReceipt),
    GitReachability(GitReachabilityReceipt),
    GitObjectAvailability(GitObjectAvailabilityReceipt),
    GitTargetScope(GitTargetScopeReceipt),
    External {
        digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl CandidateEvidencePayload {
    pub fn git_ancestry(&self) -> Option<&GitAncestryReceipt> {
        match self {
            Self::GitAncestry(receipt) | Self::GitAncestryOverride { receipt, .. } => Some(receipt),
            _ => None,
        }
    }

    pub fn git_ancestry_override(&self) -> Option<&CandidateEvidenceOperatorOverride> {
        match self {
            Self::GitAncestryOverride { override_basis, .. } => Some(override_basis),
            _ => None,
        }
    }
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
    TargetScopeEvidenceMissing,
    TargetScopeEvidenceStale,
    TargetScopeUncovered,
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
    pub const ALL: [Self; 36] = [
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
        Self::TargetScopeEvidenceMissing,
        Self::TargetScopeEvidenceStale,
        Self::TargetScopeUncovered,
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
            Self::TargetScopeEvidenceMissing => "target_scope_evidence_missing",
            Self::TargetScopeEvidenceStale => "target_scope_evidence_stale",
            Self::TargetScopeUncovered => "target_scope_uncovered",
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
            | Self::TargetScopeEvidenceMissing
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
            | Self::TargetScopeEvidenceStale
            | Self::SupersessionCycle
            | Self::SupersessionBroken
            | Self::AncestorSupersessionUnresolved
            | Self::AncestorPending
            | Self::AncestorAbandoned => LandabilityReasonClass::Bookkeeping,
            Self::TargetScopeUncovered => LandabilityReasonClass::Substantive,
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

impl CandidatePolicySnapshot {
    pub(crate) fn has_complete_reconciliation_clocks(&self) -> bool {
        self.review_policy_op_id
            .as_deref()
            .is_some_and(|op_id| !op_id.trim().is_empty())
            && self
                .landing_repository_op_id
                .as_deref()
                .is_some_and(|op_id| !op_id.trim().is_empty())
    }
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

fn merge_bases(cwd: &Path, left: &str, right: &str) -> Result<Vec<String>, String> {
    let output = git_output(cwd, &["merge-base", "--all", left, right])?;
    if !output.status.success() {
        return Err(format!(
            "git merge-base --all failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git merge-base --all returned non-UTF-8 output: {error}"))?;
    let mut bases = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    bases.sort();
    bases.dedup();
    if bases.is_empty() {
        return Err("candidate and landing target have no merge base".into());
    }
    Ok(bases)
}

fn changed_paths_without_rename_inference(
    cwd: &Path,
    base_oid: &str,
    right_oid: &str,
) -> Result<Vec<String>, String> {
    let output = git_output(
        cwd,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            base_oid,
            right_oid,
            "--",
        ],
    )?;
    if !output.status.success() {
        return Err(format!(
            "git diff --name-only failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| {
            let path = std::str::from_utf8(raw)
                .map_err(|error| format!("Git path is not UTF-8: {error}"))?;
            crate::paths::normalize(path)
                .map_err(|error| format!("Git returned a non-canonical repository path: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn resolve_mutable_ref(cwd: &Path, target_ref: &str) -> Result<String, String> {
    let full_name = git_text(cwd, &["rev-parse", "--symbolic-full-name", target_ref])?;
    if !full_name.starts_with("refs/") || full_name.lines().count() != 1 {
        return Err(format!(
            "landing target `{target_ref}` does not resolve to one mutable full ref"
        ));
    }
    Ok(full_name)
}

fn prospective_merge_tree(
    cwd: &Path,
    object_format: &str,
    target_oid: &str,
    candidate_oid: &str,
) -> Result<String, String> {
    let output = git_output(
        cwd,
        &["merge-tree", "--write-tree", target_oid, candidate_oid],
    )?;
    if !output.status.success() {
        return Err(format!(
            "prospective target merge is unavailable or conflicted: {}{}",
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git merge-tree returned non-UTF-8 output: {error}"))?;
    let tree_oid = stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .ok_or_else(|| "git merge-tree returned no prospective tree".to_string())?;
    if !validate_full_oid(object_format, tree_oid)
        || git_text(cwd, &["cat-file", "-t", tree_oid])? != "tree"
    {
        return Err("git merge-tree did not return a full tree object id".into());
    }
    Ok(tree_oid.to_string())
}

fn commit_tree(cwd: &Path, object_format: &str, commit_oid: &str) -> Result<String, String> {
    let tree_expression = format!("{commit_oid}^{{tree}}");
    let tree_oid = git_text(cwd, &["rev-parse", &tree_expression])?;
    if !validate_full_oid(object_format, &tree_oid)
        || git_text(cwd, &["cat-file", "-t", &tree_oid])? != "tree"
    {
        return Err("landing result did not resolve to a full tree object id".into());
    }
    Ok(tree_oid)
}

fn canonical_sorted_paths(paths: &[String]) -> bool {
    paths
        .iter()
        .all(|path| crate::paths::normalize(path).is_ok_and(|normalized| normalized == *path))
        && paths.windows(2).all(|pair| pair[0] < pair[1])
}

fn immediate_ref_preimage(
    cwd: &Path,
    object_format: &str,
    target_ref_full_name: &str,
    after_tip: &str,
) -> Result<String, String> {
    let output = git_output(
        cwd,
        &[
            "reflog",
            "show",
            "--format=%H",
            "-n",
            "2",
            target_ref_full_name,
        ],
    )?;
    if !output.status.success() {
        return Err(format!(
            "cannot prove immediate target preimage from reflog: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("target reflog returned non-UTF-8 output: {error}"))?;
    let entries = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if entries.len() != 2
        || entries[0] != after_tip
        || !entries
            .iter()
            .all(|oid| validate_full_oid(object_format, oid))
    {
        return Err("cannot prove immediate target preimage from two exact reflog entries".into());
    }
    Ok(entries[1].to_string())
}

#[allow(clippy::too_many_arguments)]
pub fn probe_target_scope(
    cwd: &Path,
    repository_id: &str,
    landing_repository_op_id: &str,
    object_format: &str,
    candidate_oid: &str,
    candidate_base_oid: &str,
    target_ref: &str,
) -> Result<GitTargetScopeReceipt, String> {
    let (observed_repository, observed_format, _) = repository_identity(cwd)?;
    if observed_repository != repository_id || observed_format != object_format {
        return Err(
            "target-scope repository identity does not match candidate landing repository".into(),
        );
    }
    if landing_repository_op_id.trim().is_empty() {
        return Err("target-scope evidence requires the current landing-repository binding".into());
    }
    let observed_candidate_oid = resolve_commit(cwd, candidate_oid)?;
    let observed_candidate_base_oid = resolve_commit(cwd, candidate_base_oid)?;
    if observed_candidate_oid != candidate_oid || observed_candidate_base_oid != candidate_base_oid
    {
        return Err("target-scope candidate anchors do not resolve exactly".into());
    }
    let target_ref_full_name = resolve_mutable_ref(cwd, target_ref)?;
    let observed_target_oid = resolve_commit(cwd, &target_ref_full_name)?;
    let candidate_is_ancestor_of_target = match ancestor_relation(
        cwd,
        candidate_oid,
        &observed_target_oid,
    ) {
        GitRelationKind::Ancestor => {
            return Err(
                    "landing target already contains the candidate; use out-of-band reconciliation instead"
                        .into(),
                );
        }
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };
    if candidate_is_ancestor_of_target.is_none() {
        return Err("candidate relation to landing target is unavailable or ambiguous".into());
    }
    let merge_base_oids = merge_bases(cwd, &observed_target_oid, candidate_oid)?;
    if !merge_base_oids
        .iter()
        .all(|oid| validate_full_oid(object_format, oid))
    {
        return Err("target-scope merge base is not a full object id".into());
    }
    let mut candidate_side_paths = std::collections::BTreeSet::new();
    for merge_base_oid in &merge_base_oids {
        candidate_side_paths.extend(changed_paths_without_rename_inference(
            cwd,
            merge_base_oid,
            candidate_oid,
        )?);
    }
    let prospective_merge_tree_oid =
        prospective_merge_tree(cwd, object_format, &observed_target_oid, candidate_oid)?;
    let prospective_target_effect_paths = changed_paths_without_rename_inference(
        cwd,
        &observed_target_oid,
        &prospective_merge_tree_oid,
    )?;
    let candidate_side_paths = candidate_side_paths.into_iter().collect::<Vec<_>>();
    let effective_paths = candidate_side_paths
        .iter()
        .chain(prospective_target_effect_paths.iter())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let base_is_ancestor_of_target =
        match ancestor_relation(cwd, candidate_base_oid, &observed_target_oid) {
            GitRelationKind::Ancestor => Some(true),
            GitRelationKind::NotAncestor => Some(false),
            GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
        };
    if base_is_ancestor_of_target.is_none() {
        return Err("candidate base relation to landing target is unavailable or ambiguous".into());
    }
    Ok(GitTargetScopeReceipt {
        scope_schema: GIT_TARGET_SCOPE_SCHEMA_V1,
        repository_id: observed_repository,
        landing_repository_op_id: landing_repository_op_id.to_string(),
        object_format: observed_format,
        candidate_oid: candidate_oid.to_string(),
        candidate_base_oid: candidate_base_oid.to_string(),
        target_ref: target_ref.to_string(),
        target_ref_full_name,
        observed_target_oid,
        candidate_is_ancestor_of_target,
        base_is_ancestor_of_target,
        merge_base_oids,
        prospective_merge_tree_oid,
        candidate_side_paths,
        prospective_target_effect_paths,
        effective_paths,
        git_version: git_version(cwd),
        detail: None,
    })
}

pub fn target_scope_shape_is_valid(receipt: &GitTargetScopeReceipt) -> bool {
    receipt.scope_schema == GIT_TARGET_SCOPE_SCHEMA_V1
        && !receipt.repository_id.trim().is_empty()
        && !receipt.landing_repository_op_id.trim().is_empty()
        && matches!(receipt.object_format.as_str(), "sha1" | "sha256")
        && validate_full_oid(&receipt.object_format, &receipt.candidate_oid)
        && validate_full_oid(&receipt.object_format, &receipt.candidate_base_oid)
        && !receipt.target_ref.trim().is_empty()
        && receipt.target_ref_full_name.starts_with("refs/")
        && validate_full_oid(&receipt.object_format, &receipt.observed_target_oid)
        && receipt.candidate_is_ancestor_of_target == Some(false)
        && receipt.base_is_ancestor_of_target.is_some()
        && !receipt.merge_base_oids.is_empty()
        && receipt
            .merge_base_oids
            .iter()
            .all(|oid| validate_full_oid(&receipt.object_format, oid))
        && receipt
            .merge_base_oids
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && validate_full_oid(&receipt.object_format, &receipt.prospective_merge_tree_oid)
        && canonical_sorted_paths(&receipt.candidate_side_paths)
        && canonical_sorted_paths(&receipt.prospective_target_effect_paths)
        && canonical_sorted_paths(&receipt.effective_paths)
        && receipt.effective_paths
            == receipt
                .candidate_side_paths
                .iter()
                .chain(receipt.prospective_target_effect_paths.iter())
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
}

pub fn uncovered_target_scope_paths(
    declared_paths: &[String],
    effective_paths: &[String],
) -> Vec<String> {
    effective_paths
        .iter()
        .filter(|effective| {
            !declared_paths
                .iter()
                .any(|declared| crate::paths::overlap(declared, effective))
        })
        .cloned()
        .collect()
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
    target_scope: Option<&GitTargetScopeReceipt>,
    target_scope_evidence_id: Option<&str>,
    target_scope_op_id: Option<&str>,
) -> Result<GitLandingReceipt, String> {
    let (observed_repository, observed_format, _) = repository_identity(cwd)?;
    if observed_repository != repository_id || observed_format != object_format {
        return Err("landing repository identity does not match candidate".into());
    }
    let after_tip = resolve_commit(cwd, target_ref)?;
    let after_tree_oid = commit_tree(cwd, object_format, &after_tip)?;
    let caller_before_tip = before_tip
        .map(|reference| resolve_commit(cwd, reference))
        .transpose()?;
    let (
        effective_before_tip,
        target_ref_full_name,
        landing_effect_paths,
        target_scope_evidence_id,
        target_scope_op_id,
    ) = match (target_scope, target_scope_evidence_id, target_scope_op_id) {
        (Some(target_scope), Some(target_scope_evidence_id), Some(target_scope_op_id))
            if target_scope_shape_is_valid(target_scope)
                && target_scope.repository_id == repository_id
                && target_scope.object_format == object_format
                && target_scope.candidate_oid == candidate_oid
                && target_scope.target_ref == target_ref
                && !target_scope_evidence_id.trim().is_empty()
                && !target_scope_op_id.trim().is_empty() =>
        {
            let target_ref_full_name = resolve_mutable_ref(cwd, target_ref)?;
            if target_ref_full_name != target_scope.target_ref_full_name
                || resolve_commit(cwd, &target_ref_full_name)? != after_tip
            {
                return Err("target-scope evidence is stale: target ref identity changed".into());
            }
            let immediate_before_tip =
                immediate_ref_preimage(cwd, object_format, &target_ref_full_name, &after_tip)?;
            if immediate_before_tip != target_scope.observed_target_oid {
                return Err(
                        "target-scope evidence is stale: immediate target preimage does not match target-scope evidence"
                            .into(),
                    );
            }
            if caller_before_tip
                .as_ref()
                .is_some_and(|before| before != &immediate_before_tip)
            {
                return Err(
                    "--before does not match the reflog-proved immediate target preimage".into(),
                );
            }
            let landing_effect_paths =
                changed_paths_without_rename_inference(cwd, &immediate_before_tip, &after_tip)?;
            if landing_effect_paths != target_scope.prospective_target_effect_paths {
                return Err(
                    "actual target-to-result paths do not match target-scope evidence".into(),
                );
            }
            if after_tree_oid != target_scope.prospective_merge_tree_oid {
                return Err("actual landing tree does not match target-scope evidence".into());
            }
            (
                Some(immediate_before_tip),
                Some(target_ref_full_name),
                landing_effect_paths,
                Some(target_scope_evidence_id.to_string()),
                Some(target_scope_op_id.to_string()),
            )
        }
        (None, None, None) => (caller_before_tip, None, Vec::new(), None, None),
        _ => {
            return Err(
                "landing target-scope evidence does not match candidate and target anchors".into(),
            );
        }
    };
    let relation = ancestor_relation(cwd, candidate_oid, &after_tip);
    let candidate_reachable = match relation {
        GitRelationKind::Ancestor => Some(true),
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };
    if let (Some(true), Some(target_scope)) = (candidate_reachable, target_scope) {
        let exact_transition = if after_tip == candidate_oid {
            ancestor_relation(cwd, &target_scope.observed_target_oid, candidate_oid)
                == GitRelationKind::Ancestor
        } else {
            commit_parents(cwd, &after_tip)?
                .first()
                .is_some_and(|parent| parent == &target_scope.observed_target_oid)
        };
        if !exact_transition {
            return Err(
                "target-scope evidence is stale: the observed target is not the exact landing preimage"
                    .into(),
            );
        }
    }
    Ok(GitLandingReceipt {
        repository_id: observed_repository,
        object_format: observed_format,
        candidate_oid: candidate_oid.to_string(),
        target_ref: target_ref.to_string(),
        target_ref_full_name,
        before_tip: effective_before_tip,
        after_tip,
        after_tree_oid: Some(after_tree_oid),
        landing_effect_paths,
        candidate_reachable,
        authorization_op_id: authorization_op_id.to_string(),
        basis_op_ids,
        target_scope_evidence_id,
        target_scope_op_id,
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
            LandabilityReasonCode::TargetScopeUncovered,
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
            LandabilityReasonCode::TargetScopeEvidenceMissing,
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
