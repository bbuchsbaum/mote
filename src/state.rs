//! Derived state from replaying ops.
//!
//! All state lives in memory; we never persist `state.rs` data structures to
//! disk in v0.2 (snapshots are deferred). The reducer in `crate::reducer`
//! mutates this struct in filename order.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Serialize, Serializer};

use crate::candidate::{
    AuthorizationStatus, CandidateContainmentBasis, CandidateEvidencePayload, CandidatePhase,
    CandidatePolicySnapshot, CandidateReconciliationAuthority, CandidateReviewQualification,
    CandidateReviewStatus, CandidateSnapshotProvenance, CandidateSupersessionAuthority,
    EvidenceOutcome, EvidenceRequirement, GIT_ANCESTRY_EVIDENCE, GIT_RELATION_SCHEMA_V2,
    GitCandidateRelation, GitRelationKind, Landability, LandabilityReason, LandabilityReasonCode,
    NamedReviewStatus, ReviewVerdict, RoleReviewEntryStatus, RoleReviewRequirementStatus,
};
use crate::op::{DecisionQuestionAction, DiscussionReference, Status};
use crate::role::{
    RoleAssignmentClock, RoleAssignmentDisposition, RoleCoverage, RoleDemandSource, RoleExclusion,
};

#[derive(Debug, Clone)]
pub struct Bead {
    pub id: String,
    pub title: String,
    pub status: Status,
    pub priority: i32,
    pub body: String,
    pub assignee: Option<String>,
    pub tags: BTreeSet<String>,
    /// (parent_id, kind) — current bead is blocked on `parent_id`.
    pub deps: BTreeSet<(String, String)>,
    /// (parent_id, kind) — non-blocking hierarchy/containment relationships.
    pub rels: BTreeSet<(String, String)>,
    pub clock: BTreeMap<String, String>,
    pub notes: Vec<Note>,
    pub claim: Option<ClaimState>,
    pub created_at_op: String,
    pub created_at_ts: String,
    pub deleted_at_ts: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClaimState {
    pub claimed_by: String,
    pub claim_clock: String,
    /// RFC3339 microsecond UTC string. Comparable lexicographically against any
    /// other RFC3339 timestamp produced by `ids::format_rfc3339`.
    pub lease_until_ts: String,
}

impl ClaimState {
    /// True iff the lease is still live as-of `now_ts` (RFC3339 string).
    pub fn is_live(&self, now_ts: &str) -> bool {
        now_ts < self.lease_until_ts.as_str()
    }
}

impl Bead {
    pub fn is_deleted(&self) -> bool {
        self.deleted_at_ts.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct Note {
    pub op_id: String,
    pub note_kind: String,
    pub actor: String,
    pub ts: String,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub beads: BTreeMap<String, Bead>,
    /// Per-entity history in filename order. Includes both accepted and rejected
    /// ops so `mote history --include-rejected` is a simple lookup.
    pub history: BTreeMap<String, Vec<HistoryEntry>>,
    /// Ops that did not bind to any entity (e.g. malformed JSON, or message
    /// operations without an `entity` reference) live here.
    pub orphan_history: Vec<HistoryEntry>,
    /// All messages, indexed by `msg_id`. Both unacked and acked.
    pub messages: BTreeMap<String, MsgRecord>,
    /// Public discussion-board posts, indexed by `post_id`.
    pub board_posts: BTreeMap<String, BoardPostRecord>,
    /// Discussion-board posts indexed by the accepted board_post op id.
    pub board_post_op_index: BTreeMap<String, String>,
    /// Structured cited decisions, indexed by their decision/post id.
    pub board_decisions: BTreeMap<String, BoardDecisionRecord>,
    /// Decision questions, including terminal records, indexed by question id.
    pub board_questions: BTreeMap<String, DecisionQuestionRecord>,
    /// Public discussion-board topics, indexed by topic name.
    pub board_topics: BTreeMap<String, BoardTopicRecord>,
    /// Per-actor discussion-board read cursor, as the latest seen board_post op id.
    pub board_read_cursors: BTreeMap<String, String>,
    /// Per-(actor, topic) discussion-board read cursor.
    pub board_topic_read_cursors: BTreeMap<(String, String), String>,
    /// Current explicit topic-watch register per actor and topic.
    pub board_topic_watches: BTreeMap<(String, String), BoardTopicWatchRecord>,
    /// All reservations, indexed by `reservation_id`. Both live and closed.
    pub reservations: BTreeMap<String, ReservationState>,
    /// All session leases, indexed by `session_id`. Both live and ended.
    pub sessions: BTreeMap<String, SessionRecord>,
    /// Actor-scoped retry registry for heartbeat and status operations.
    pub session_idempotency: BTreeMap<(String, String), SessionIdempotencyRecord>,
    /// Immutable role definitions, indexed by role id.
    pub roles: BTreeMap<String, RoleRecord>,
    /// Store-unique immutable role names to ids. Retired names remain reserved.
    pub role_names: BTreeMap<String, String>,
    /// All bounded role assignments, including terminal assignments.
    pub role_assignments: BTreeMap<String, RoleAssignmentRecord>,
    /// Actor-scoped retry registry for role mutations.
    pub role_idempotency: BTreeMap<(String, String), RoleIdempotencyRecord>,
    /// Candidate protocol state, derived exclusively from candidate ops.
    pub candidates: BTreeMap<String, CandidateRecord>,
    /// Latest explicit ancestry rows contributed by each subject candidate for
    /// each known candidate. The directed key makes the two independently
    /// produced observations of one unordered pair available without allowing
    /// an omitted row to erase an immutable fact.
    pub candidate_pair_evidence: BTreeMap<(String, String), CandidatePairEvidenceRecord>,
    /// Sender-scoped retry registry. Same key plus same digest is a no-op;
    /// reusing a key for a different action is rejected.
    pub candidate_idempotency: BTreeMap<(String, String), CandidateIdempotencyRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateRecord {
    pub candidate_id: String,
    pub entity: String,
    pub proposer: String,
    pub proposal_op_id: String,
    pub store_id: String,
    /// Repository where proposal ancestry was observed.
    pub repository_id: String,
    /// Repository against which object availability and landing are checked.
    pub landing_repository_id: String,
    pub landing_repository_op_id: String,
    pub landing_repository_bindings: Vec<CandidateLandingRepositoryBindingRecord>,
    pub object_source: Option<crate::candidate::CandidateObjectSource>,
    pub object_availability_required: bool,
    pub object_format: String,
    pub commit_oid: String,
    pub base_oid: String,
    pub parent_oids: Vec<String>,
    pub paths: Vec<String>,
    pub authorizer: String,
    pub review_policy_version: u32,
    pub review_policy_op_id: String,
    pub reviewers: Vec<String>,
    pub role_review_requirements: Vec<crate::candidate::RoleReviewRequirement>,
    pub review_policy_amendments: Vec<CandidateReviewPolicyAmendmentRecord>,
    pub evidence_requirements: Vec<EvidenceRequirement>,
    pub evidence_refs: Vec<String>,
    pub phase: CandidatePhase,
    pub phase_op_id: String,
    pub successor_id: Option<String>,
    pub supersession: Option<CandidateSupersessionRecord>,
    pub reviews: BTreeMap<String, CandidateReviewRecord>,
    #[serde(serialize_with = "serialize_candidate_evidence")]
    pub evidence: BTreeMap<(String, String), CandidateEvidenceRecord>,
    pub authorization: Option<CandidateAuthorizationRecord>,
    pub landed: Option<CandidateLandedRecord>,
    pub reconciled: Option<CandidateReconciledRecord>,
}

fn serialize_candidate_evidence<S>(
    evidence: &BTreeMap<(String, String), CandidateEvidenceRecord>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    evidence.values().collect::<Vec<_>>().serialize(serializer)
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateReviewRecord {
    pub reviewer: String,
    pub qualification: crate::candidate::CandidateReviewQualification,
    pub verdict: ReviewVerdict,
    pub body: Option<String>,
    pub evidence_refs: Vec<String>,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateReviewPolicyAmendmentRecord {
    pub actor: String,
    pub before_named_reviewers: Vec<String>,
    pub after_named_reviewers: Vec<String>,
    pub added_reviewers: Vec<String>,
    pub removed_reviewers: Vec<String>,
    pub reason: String,
    pub prior_policy_op_id: String,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateLandingRepositoryBindingRecord {
    pub actor: String,
    pub before_repository_id: String,
    pub after_repository_id: String,
    pub object_source: Option<crate::candidate::CandidateObjectSource>,
    pub reason: String,
    pub prior_binding_op_id: String,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateEvidenceRecord {
    pub producer: String,
    pub producer_tool: String,
    pub evidence_id: String,
    pub name: String,
    pub evidence_kind: String,
    pub candidate_oid: String,
    pub outcome: EvidenceOutcome,
    pub payload: CandidateEvidencePayload,
    pub refs: Vec<String>,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone)]
pub struct CandidatePairEvidenceRecord {
    pub subject_candidate_id: String,
    pub subject_proposal_op_id: String,
    pub subject_store_id: String,
    pub repository_id: String,
    pub object_format: String,
    pub subject_commit_oid: String,
    pub subject_base_oid: String,
    pub relation_schema: u32,
    pub known_candidate_id: String,
    pub relations: Vec<GitCandidateRelation>,
    pub covered_candidates: Vec<(String, String)>,
    pub producer_snapshot: Option<Box<CandidateSnapshotProvenance>>,
    pub evidence_op_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CandidatePairFact {
    other_to_candidate_base: GitRelationKind,
    other_to_candidate_tip: GitRelationKind,
}

#[derive(Debug, Clone)]
enum PairSourceObservation {
    Absent(String),
    Unknown(String),
    Conflict(String),
    Determinate {
        fact: CandidatePairFact,
        source: String,
        evidence_op_id: String,
    },
}

#[derive(Debug, Clone)]
enum CandidatePairResolution {
    Stale(String),
    Ambiguous(String),
    Resolved {
        fact: CandidatePairFact,
        evidence_op_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateAuthorizationRecord {
    pub status: AuthorizationStatus,
    pub grantees: Vec<String>,
    pub conditions: Vec<String>,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateLandedRecord {
    pub actor: String,
    pub evidence_id: String,
    pub authorization_op_id: String,
    pub target_ref: String,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateReconciledRecord {
    pub actor: String,
    pub authority: CandidateReconciliationAuthority,
    pub override_basis: Option<crate::candidate::CandidateOperatorOverride>,
    pub repository_bridge: Option<crate::candidate::CandidateReconciliationRepositoryBridge>,
    pub evidence_id: String,
    pub target_ref: String,
    pub target_oid: String,
    pub policy_snapshot: CandidatePolicySnapshot,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateSupersessionRecord {
    pub successor_id: String,
    pub actor: String,
    pub authority: CandidateSupersessionAuthority,
    pub containment_evidence_op_ids: Vec<String>,
    pub reviews_not_carried: Vec<CandidateSupersededReviewRecord>,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateSupersededReviewRecord {
    pub reviewer: String,
    pub qualification: CandidateReviewQualification,
    pub verdict: ReviewVerdict,
    pub review_op_id: String,
}

#[derive(Debug, Clone)]
pub struct CandidateIdempotencyRecord {
    pub candidate_id: String,
    pub digest: String,
    pub op_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleRecord {
    pub role_id: String,
    pub name: String,
    pub remit: String,
    pub exclusions: Vec<RoleExclusion>,
    pub assignment_authorities: Vec<String>,
    pub capacity: u32,
    pub minimum_active: u32,
    pub defined_by: String,
    pub definition_op_id: String,
    pub defined_ts: String,
    pub retired: Option<RoleRetirementRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleRetirementRecord {
    pub actor: String,
    pub reason: Option<String>,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RoleAssignmentRecord {
    pub assignment_id: String,
    pub role_id: String,
    pub holder_actor: String,
    pub holder_session_id: String,
    pub session_lease_op_id: String,
    pub assigned_by: String,
    pub assigned_op_id: String,
    pub assigned_ts: String,
    pub ttl_s: u32,
    pub lease_until_ts: String,
    pub clock_op_id: String,
    pub last_updated_by: String,
    pub last_updated_ts: String,
    pub released_by: Option<String>,
    pub release_reason: Option<String>,
    pub released_op_id: Option<String>,
    pub released_ts: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoleIdempotencyRecord {
    pub role_id: String,
    pub digest: String,
    pub op_id: String,
}

#[derive(Debug, Clone)]
pub struct ReservationState {
    pub reservation_id: String,
    pub actor: String,
    pub entity: String,
    pub paths: Vec<String>,
    pub ttl_s: u32,
    pub opened_op_id: String,
    /// Latest accepted open/close/adopt transition for CAS.
    pub clock: String,
    pub opened_ts: String,
    pub lease_until_ts: String,
    pub closed_paths: BTreeSet<String>,
    pub adoptions: Vec<ReservationAdoption>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReservationAdoption {
    pub op_id: String,
    pub ts: String,
    pub from_actor: String,
    pub from_entity: String,
    pub to_actor: String,
    pub to_entity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseDisposition {
    Active,
    Orphaned,
    Expired,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationExpiryPhase {
    Expiring,
    Expired,
}

impl ReservationExpiryPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expiring => "expiring",
            Self::Expired => "expired",
        }
    }
}

impl LeaseDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Orphaned => "orphaned",
            Self::Expired => "expired",
            Self::Closed => "closed",
        }
    }
}

impl ReservationState {
    pub fn is_live(&self, now_ts: &str) -> bool {
        now_ts < self.lease_until_ts.as_str()
    }

    /// Paths still under reservation (not closed).
    pub fn live_paths(&self) -> Vec<&str> {
        self.paths
            .iter()
            .filter(|p| !self.closed_paths.contains(p.as_str()))
            .map(String::as_str)
            .collect()
    }

    /// `true` iff the reservation is live AND has at least one un-closed path.
    pub fn is_active(&self, now_ts: &str) -> bool {
        self.is_live(now_ts) && !self.live_paths().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Open,
    Responded,
    Declined,
    Resolved,
}

impl RequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Responded => "responded",
            Self::Declined => "declined",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "responded" => Some(Self::Responded),
            "declined" => Some(Self::Declined),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MsgRecord {
    pub msg_id: String,
    pub from: String,
    pub to: String,
    pub entity: Option<String>,
    pub reservation: Option<String>,
    pub msg_kind: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub correlation_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub answers: Vec<String>,
    pub require_live: bool,
    pub recipient_presence: MsgPresenceEvidence,
    /// Present only on root request messages.
    pub request_state: Option<RequestState>,
    pub response_msg_id: Option<String>,
    pub response_post_id: Option<String>,
    pub resolved_op_id: Option<String>,
    pub resolved_ts: Option<String>,
    pub sent_ts: String,
    pub sent_op_id: String,
    pub ack_op_id: Option<String>,
    pub ack_ts: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MsgPresenceEvidence {
    pub state: String,
    pub source: String,
    pub reason: String,
    pub as_of_ts: String,
}

/// Whether a discussion post or topic still needs tracker action.
///
/// `Open` is the implicit default: nothing has been declared about the target,
/// which is not the same as "needs a bead". Only `NeedsBead` is an explicit
/// claim that the discussion is actionable and unrouted, so
/// `mote discuss unrouted` answers a question about declared state rather than
/// guessing from prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteState {
    #[default]
    Open,
    NeedsBead,
    Routed,
    Resolved,
}

impl RouteState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::NeedsBead => "needs_bead",
            Self::Routed => "routed",
            Self::Resolved => "resolved",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "needs_bead" => Some(Self::NeedsBead),
            "routed" => Some(Self::Routed),
            "resolved" => Some(Self::Resolved),
            _ => None,
        }
    }
}

/// Derived routing state attached to a post or a topic.
#[derive(Debug, Clone, Default)]
pub struct RouteRecord {
    pub state: RouteState,
    /// Beads this discussion target has been linked to, in id order.
    pub issues: BTreeSet<String>,
    pub updated_by: Option<String>,
    pub updated_ts: Option<String>,
    pub updated_op_id: Option<String>,
}

impl RouteRecord {
    /// `true` when the target has been declared actionable but carries no bead.
    pub fn needs_action(&self) -> bool {
        self.state == RouteState::NeedsBead
    }
}

#[derive(Debug, Clone)]
pub struct BoardPostRecord {
    pub post_id: String,
    pub from: String,
    pub topic: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub post_kind: String,
    pub answers: Vec<String>,
    pub explicit_notify: Vec<String>,
    pub notification_recipients: Vec<String>,
    pub idempotency_key: Option<String>,
    pub sticky: bool,
    pub sticky_op_id: Option<String>,
    pub superseded_by: Option<String>,
    pub superseded_op_id: Option<String>,
    pub supersedes: Vec<String>,
    pub retracted: bool,
    pub retraction_reason: Option<String>,
    pub retracted_op_id: Option<String>,
    pub route: RouteRecord,
    pub sent_ts: String,
    pub sent_op_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BoardTopicWatchRecord {
    pub actor: String,
    pub topic: String,
    pub watching: bool,
    pub updated_ts: String,
    pub updated_op_id: String,
}

impl BoardPostRecord {
    pub fn disposition(&self) -> &'static str {
        if self.retracted {
            "retracted"
        } else if self.superseded_by.is_some() {
            "superseded"
        } else {
            "active"
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BoardDecisionRecord {
    pub decision_id: String,
    pub post_id: String,
    pub topic: String,
    pub agreed_post_ids: Vec<String>,
    pub references: Vec<DiscussionReference>,
    pub question_ids: Vec<String>,
    pub actor: String,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionQuestionStatus {
    Open,
    Deferred,
    Superseded,
    Closed,
}

impl DecisionQuestionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Deferred => "deferred",
            Self::Superseded => "superseded",
            Self::Closed => "closed",
        }
    }

    pub const fn unresolved(self) -> bool {
        matches!(self, Self::Open | Self::Deferred)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionQuestionTransitionRecord {
    pub action: DecisionQuestionAction,
    pub actor: String,
    pub expect_question: String,
    pub references: Vec<DiscussionReference>,
    pub note: Option<String>,
    pub successor_question_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub op_id: String,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionQuestionRecord {
    pub question_id: String,
    pub decision_id: String,
    pub topic: String,
    pub text: String,
    pub opened_by: String,
    pub opened_op_id: String,
    pub opened_ts: String,
    pub position: usize,
    pub status: DecisionQuestionStatus,
    pub clock_op_id: String,
    pub successor_question_id: Option<String>,
    pub transitions: Vec<DecisionQuestionTransitionRecord>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DecisionQuestionCounts {
    pub total: usize,
    pub open: usize,
    pub deferred: usize,
    pub superseded: usize,
    pub closed: usize,
    pub unresolved: usize,
}

#[derive(Debug, Clone)]
pub struct BoardTopicRecord {
    pub topic: String,
    pub title: String,
    pub body: String,
    pub created_by: String,
    pub created_ts: String,
    pub created_op_id: String,
    pub explicit: bool,
    pub last_activity_ts: String,
    pub last_activity_op_id: String,
    pub post_count: usize,
    pub sticky_count: usize,
    pub decision_count: usize,
    /// Most recent `summary` post, i.e. the topic's pinned current state.
    pub summary_post_id: Option<String>,
    pub route: RouteRecord,
}

/// A TTL-bounded session lease. Multiple concurrent sessions may share one
/// actor name; the lease is what makes them individually visible.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub session_id: String,
    pub actor: String,
    pub label: Option<String>,
    pub pid: Option<u32>,
    pub started_label: Option<String>,
    pub started_pid: Option<u32>,
    pub ttl_s: u32,
    pub started_ttl_s: u32,
    pub started_ts: String,
    pub started_op_id: String,
    pub started_lease_until_ts: String,
    pub last_heartbeat_ts: String,
    pub last_heartbeat_op_id: String,
    pub lease_until_ts: String,
    /// Accepted renewals after the start operation, retained so projections
    /// can answer at an injected historical `as_of_ts`.
    pub heartbeats: Vec<SessionHeartbeatRecord>,
    pub intent: Option<SessionIntentRecord>,
    /// Full accepted intent history; `intent` remains the current convenience
    /// register while this vector makes historical projections deterministic.
    pub intents: Vec<SessionIntentRecord>,
    pub ended_ts: Option<String>,
    pub ended_op_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionHeartbeatRecord {
    pub ts: String,
    pub op_id: String,
    pub ttl_s: u32,
    pub lease_until_ts: String,
    pub label: Option<String>,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionIntentRecord {
    pub state: String,
    pub message: Option<String>,
    pub issue: Option<String>,
    pub set_ts: String,
    pub set_op_id: String,
}

#[derive(Debug, Clone)]
pub struct SessionIdempotencyRecord {
    pub op_id: String,
    pub kind: String,
    pub digest: String,
}

impl SessionRecord {
    /// Live means not explicitly ended and not past its lease.
    pub fn is_live(&self, now_ts: &str) -> bool {
        self.ended_ts.is_none() && now_ts < self.lease_until_ts.as_str()
    }
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub op_id: String,
    pub kind: String,
    pub actor: String,
    pub ts: String,
    pub accepted: bool,
    pub reason: Option<String>,
}

impl HistoryEntry {
    pub fn accepted(op_id: &str, kind: &str, actor: &str, ts: &str) -> Self {
        Self {
            op_id: op_id.to_string(),
            kind: kind.to_string(),
            actor: actor.to_string(),
            ts: ts.to_string(),
            accepted: true,
            reason: None,
        }
    }

    pub fn rejected(op_id: &str, kind: &str, actor: &str, ts: &str, reason: String) -> Self {
        Self {
            op_id: op_id.to_string(),
            kind: kind.to_string(),
            actor: actor.to_string(),
            ts: ts.to_string(),
            accepted: false,
            reason: Some(reason),
        }
    }
}

fn candidate_reason(
    code: LandabilityReasonCode,
    subject: Option<impl Into<String>>,
    detail: impl Into<String>,
) -> LandabilityReason {
    LandabilityReason::new(code, subject, detail)
}

fn snapshot_summary(snapshot: &CandidateSnapshotProvenance) -> String {
    format!(
        "producer snapshot store={} replayed_ops={} op_ids_digest={} git_head={} uncommitted_ops={}",
        snapshot.store_id,
        snapshot.replayed_op_count,
        snapshot.replayed_op_ids_digest,
        snapshot.store_git_head.as_deref().unwrap_or("unavailable"),
        snapshot
            .uncommitted_op_count
            .map_or_else(|| "unavailable".to_string(), |count| count.to_string())
    )
}

fn snapshot_omission_detail(
    source: &str,
    snapshot: &CandidateSnapshotProvenance,
    missing_proposal_op: &str,
) -> String {
    format!(
        "{source} was published after proposal op {missing_proposal_op}, but proposal op {missing_proposal_op} is absent from the declared {}; repeating the refresh in this producer checkout cannot repair the target until the checkout receives the missing proposal op; refresh from a store snapshot containing both exact proposal ops",
        snapshot_summary(snapshot)
    )
}

impl State {
    pub fn push_history(&mut self, entity: Option<&str>, entry: HistoryEntry) {
        match entity {
            Some(e) => self.history.entry(e.to_string()).or_default().push(entry),
            None => self.orphan_history.push(entry),
        }
    }

    pub fn reservation_disposition(
        &self,
        reservation: &ReservationState,
        now_ts: &str,
    ) -> LeaseDisposition {
        if reservation.live_paths().is_empty() {
            LeaseDisposition::Closed
        } else if !reservation.is_live(now_ts) {
            LeaseDisposition::Expired
        } else {
            match self.reservation_binding_kind(reservation) {
                "bead" => {
                    if self
                        .beads
                        .get(&reservation.entity)
                        .is_none_or(|bead| bead.is_deleted() || bead.status == Status::Closed)
                    {
                        LeaseDisposition::Orphaned
                    } else {
                        LeaseDisposition::Active
                    }
                }
                "candidate" => {
                    let candidate = &self.candidates[&reservation.entity];
                    let invalidated_by_revoke =
                        self.history
                            .get(&reservation.entity)
                            .is_some_and(|history| {
                                history.iter().any(|entry| {
                                    entry.accepted
                                        && entry.kind == "candidate_revoke"
                                        && entry.op_id > reservation.opened_op_id
                                })
                            });
                    if candidate.phase != CandidatePhase::Pending || invalidated_by_revoke {
                        LeaseDisposition::Orphaned
                    } else {
                        LeaseDisposition::Active
                    }
                }
                _ => LeaseDisposition::Orphaned,
            }
        }
    }

    pub fn reservation_binding_kind(&self, reservation: &ReservationState) -> &'static str {
        if self.beads.contains_key(&reservation.entity) {
            "bead"
        } else if self.candidates.contains_key(&reservation.entity) {
            "candidate"
        } else {
            "missing"
        }
    }

    pub fn reservation_expiry_phase(
        &self,
        reservation: &ReservationState,
        now_ts: &str,
    ) -> Option<ReservationExpiryPhase> {
        if reservation.live_paths().is_empty() {
            return None;
        }
        let now: jiff::Timestamp = now_ts.parse().ok()?;
        let deadline: jiff::Timestamp = reservation.lease_until_ts.parse().ok()?;
        if now >= deadline {
            return Some(ReservationExpiryPhase::Expired);
        }
        let warning_at: jiff::Timestamp = self.reservation_warning_ts(reservation)?.parse().ok()?;
        (now >= warning_at).then_some(ReservationExpiryPhase::Expiring)
    }

    pub fn reservation_warning_ts(&self, reservation: &ReservationState) -> Option<String> {
        let deadline: jiff::Timestamp = reservation.lease_until_ts.parse().ok()?;
        let warning_seconds = (reservation.ttl_s / 10).clamp(1, 300);
        let warning_at = deadline
            .checked_sub(jiff::SignedDuration::from_secs(warning_seconds.into()))
            .ok()?;
        Some(crate::ids::format_rfc3339(warning_at))
    }

    pub fn resolve_role(&self, id_or_name: &str) -> Option<&RoleRecord> {
        self.roles.get(id_or_name).or_else(|| {
            self.role_names
                .get(id_or_name)
                .and_then(|role_id| self.roles.get(role_id))
        })
    }

    pub fn role_assignments_for(&self, role_id: &str) -> Vec<&RoleAssignmentRecord> {
        self.role_assignments
            .values()
            .filter(|assignment| assignment.role_id == role_id)
            .collect()
    }

    pub fn role_assignment_disposition(
        &self,
        assignment: &RoleAssignmentRecord,
        as_of_ts: &str,
    ) -> RoleAssignmentDisposition {
        if assignment.assigned_ts.as_str() > as_of_ts {
            return RoleAssignmentDisposition::NotStarted;
        }
        let Some(session) = self.sessions.get(&assignment.holder_session_id) else {
            return RoleAssignmentDisposition::SessionMissing;
        };
        let mut terminal: Vec<(&str, u8, RoleAssignmentDisposition)> = Vec::new();
        if assignment.lease_until_ts.as_str() <= as_of_ts {
            terminal.push((
                assignment.lease_until_ts.as_str(),
                2,
                RoleAssignmentDisposition::Expired,
            ));
        }
        if let Some(ended_ts) = session
            .ended_ts
            .as_deref()
            .filter(|ended_ts| *ended_ts <= as_of_ts)
        {
            terminal.push((ended_ts, 1, RoleAssignmentDisposition::SessionEnded));
        }
        if let Some(released_ts) = assignment
            .released_ts
            .as_deref()
            .filter(|released_ts| *released_ts <= as_of_ts)
        {
            terminal.push((released_ts, 0, RoleAssignmentDisposition::Released));
        }
        terminal
            .into_iter()
            .min_by(|left, right| left.0.cmp(right.0).then_with(|| left.1.cmp(&right.1)))
            .map(|(_, _, disposition)| disposition)
            .unwrap_or(RoleAssignmentDisposition::Active)
    }

    pub fn role_active_assignments(
        &self,
        role_id: &str,
        as_of_ts: &str,
    ) -> Vec<&RoleAssignmentRecord> {
        self.role_assignments_for(role_id)
            .into_iter()
            .filter(|assignment| {
                self.role_assignment_disposition(assignment, as_of_ts)
                    == RoleAssignmentDisposition::Active
            })
            .collect()
    }

    pub fn role_active_assignment_clocks(
        &self,
        role_id: &str,
        as_of_ts: &str,
    ) -> Vec<RoleAssignmentClock> {
        let mut clocks: Vec<RoleAssignmentClock> = self
            .role_active_assignments(role_id, as_of_ts)
            .into_iter()
            .map(|assignment| RoleAssignmentClock {
                assignment_id: assignment.assignment_id.clone(),
                clock_op_id: assignment.clock_op_id.clone(),
            })
            .collect();
        clocks.sort();
        clocks
    }

    pub fn role_assignment_clocks(&self, role_id: &str) -> Vec<RoleAssignmentClock> {
        let mut clocks: Vec<RoleAssignmentClock> = self
            .role_assignments_for(role_id)
            .into_iter()
            .map(|assignment| RoleAssignmentClock {
                assignment_id: assignment.assignment_id.clone(),
                clock_op_id: assignment.clock_op_id.clone(),
            })
            .collect();
        clocks.sort();
        clocks
    }

    pub fn role_coverage(&self, role_id: &str, as_of_ts: &str) -> Option<RoleCoverage> {
        let role = self.roles.get(role_id)?;
        let active = self.role_active_assignments(role_id, as_of_ts);
        let active_assignment_ids = active
            .iter()
            .map(|assignment| assignment.assignment_id.clone())
            .collect::<Vec<_>>();
        let active_count = active_assignment_ids.len() as u32;
        let mut demand_sources = (role.minimum_active > 0)
            .then(|| RoleDemandSource {
                kind: "standing".into(),
                source_id: role.role_id.clone(),
                required: role.minimum_active,
            })
            .into_iter()
            .collect::<Vec<_>>();
        for candidate in self
            .candidates
            .values()
            .filter(|candidate| candidate.phase == CandidatePhase::Pending)
        {
            let Some(status) = self.candidate_review_status(&candidate.candidate_id, as_of_ts)
            else {
                continue;
            };
            for requirement in status
                .roles
                .iter()
                .filter(|requirement| requirement.role_id == role_id && !requirement.satisfied)
            {
                demand_sources.push(RoleDemandSource {
                    kind: "candidate_review".into(),
                    source_id: candidate.candidate_id.clone(),
                    required: requirement.required_approvals,
                });
            }
        }
        let demanded_count = demand_sources
            .iter()
            .map(|source| source.required)
            .max()
            .unwrap_or(0);
        Some(RoleCoverage {
            as_of_ts: as_of_ts.to_string(),
            active_assignment_ids,
            active_count,
            capacity: role.capacity,
            minimum_active: role.minimum_active,
            demanded_count,
            available_capacity: role.capacity.saturating_sub(active_count),
            coverage_shortfall: demanded_count.saturating_sub(active_count),
            vacant: active_count == 0,
            demand_sources,
        })
    }

    pub fn active_role_assignments_for_actor(
        &self,
        actor: &str,
        as_of_ts: &str,
    ) -> Vec<&RoleAssignmentRecord> {
        self.role_assignments
            .values()
            .filter(|assignment| {
                assignment.holder_actor == actor
                    && self.role_assignment_disposition(assignment, as_of_ts)
                        == RoleAssignmentDisposition::Active
            })
            .collect()
    }

    pub(crate) fn candidate_role_review_eligibility(
        &self,
        candidate_id: &str,
        actor: &str,
        role_id: &str,
        assignment_id: &str,
        as_of_ts: &str,
    ) -> Result<(), (String, String)> {
        let candidate = self.candidates.get(candidate_id).ok_or_else(|| {
            (
                "candidate_missing".into(),
                "candidate does not exist".into(),
            )
        })?;
        let requirement = candidate
            .role_review_requirements
            .iter()
            .find(|requirement| requirement.role_id == role_id)
            .ok_or_else(|| {
                (
                    "role_not_required".into(),
                    format!("role {role_id} is not required by this candidate"),
                )
            })?;
        if candidate.proposer == actor {
            return Err((
                "candidate_self_review".into(),
                "candidate proposer cannot consume a role review slot".into(),
            ));
        }
        let role = self.roles.get(role_id).ok_or_else(|| {
            (
                "role_missing".into(),
                format!("required role {role_id} does not exist"),
            )
        })?;
        if role.definition_op_id != requirement.definition_op_id {
            return Err((
                "role_definition_mismatch".into(),
                format!(
                    "role definition is {}, policy requires {}",
                    role.definition_op_id, requirement.definition_op_id
                ),
            ));
        }
        if role
            .retired
            .as_ref()
            .is_some_and(|retirement| retirement.ts.as_str() <= as_of_ts)
        {
            return Err((
                "role_retired".into(),
                format!("required role {} is retired", role.name),
            ));
        }
        let assignment = self.role_assignments.get(assignment_id).ok_or_else(|| {
            (
                "assignment_missing".into(),
                format!("role assignment {assignment_id} does not exist"),
            )
        })?;
        if assignment.role_id != role_id || assignment.holder_actor != actor {
            return Err((
                "assignment_mismatch".into(),
                format!("assignment {assignment_id} does not bind actor {actor} to role {role_id}"),
            ));
        }
        if assignment.assigned_ts.as_str() > as_of_ts {
            return Err((
                "assignment_not_started".into(),
                format!(
                    "assignment {assignment_id} was created at {}, after {as_of_ts}",
                    assignment.assigned_ts
                ),
            ));
        }
        let disposition = self.role_assignment_disposition(assignment, as_of_ts);
        if disposition != RoleAssignmentDisposition::Active {
            return Err((
                "assignment_inactive".into(),
                format!(
                    "assignment {assignment_id} is {} at {as_of_ts}",
                    disposition.as_str()
                ),
            ));
        }

        for exclusion in &role.exclusions {
            let excluded = match exclusion.code {
                crate::role::RoleExclusionCode::ConcurrentRole => false,
                crate::role::RoleExclusionCode::CandidateProposer => candidate.proposer == actor,
                crate::role::RoleExclusionCode::CandidateAuthorizer => {
                    candidate.authorizer == actor
                }
                crate::role::RoleExclusionCode::CandidateNamedReviewer => {
                    candidate.reviewers.iter().any(|reviewer| reviewer == actor)
                }
                crate::role::RoleExclusionCode::CandidateEvidenceProducer => candidate
                    .evidence
                    .values()
                    .any(|evidence| evidence.ts.as_str() <= as_of_ts && evidence.producer == actor),
                crate::role::RoleExclusionCode::CandidatePathReservation => {
                    self.reservations.values().any(|reservation| {
                        reservation.actor == actor
                            && reservation.opened_ts.as_str() <= as_of_ts
                            && self.reservation_disposition(reservation, as_of_ts)
                                == LeaseDisposition::Active
                            && reservation.live_paths().iter().any(|reserved| {
                                candidate
                                    .paths
                                    .iter()
                                    .any(|path| crate::paths::overlap(reserved, path))
                            })
                    })
                }
            };
            if excluded {
                return Err((
                    format!("excluded_{}", exclusion.code.as_str()),
                    format!(
                        "role {} excludes actor {actor} because {} applies",
                        role.name,
                        exclusion.code.as_str()
                    ),
                ));
            }
        }
        Ok(())
    }

    pub fn candidate_review_status(
        &self,
        candidate_id: &str,
        as_of_ts: &str,
    ) -> Option<CandidateReviewStatus> {
        let candidate = self.candidates.get(candidate_id)?;
        let named = candidate
            .reviewers
            .iter()
            .map(|reviewer| {
                let review = candidate.reviews.get(reviewer).filter(|review| {
                    review.ts.as_str() <= as_of_ts
                        && matches!(
                            review.qualification,
                            CandidateReviewQualification::NamedReviewer
                        )
                        && !candidate.review_policy_amendments.iter().any(|amendment| {
                            amendment.op_id > review.op_id
                                && amendment.ts.as_str() <= as_of_ts
                                && !amendment.after_named_reviewers.contains(reviewer)
                        })
                });
                NamedReviewStatus {
                    reviewer: reviewer.clone(),
                    verdict: review.map(|review| review.verdict),
                    review_op_id: review.map(|review| review.op_id.clone()),
                    satisfied: review
                        .is_some_and(|review| review.verdict == ReviewVerdict::Approve),
                }
            })
            .collect::<Vec<_>>();

        let roles = candidate
            .role_review_requirements
            .iter()
            .map(|requirement| {
                let mut reviews = candidate
                    .reviews
                    .values()
                    .filter_map(|review| {
                        if review.ts.as_str() > as_of_ts {
                            return None;
                        }
                        let CandidateReviewQualification::RoleAssignment {
                            role_id,
                            assignment_id,
                            ..
                        } = &review.qualification
                        else {
                            return None;
                        };
                        if role_id != &requirement.role_id {
                            return None;
                        }
                        let eligibility = self.candidate_role_review_eligibility(
                            candidate_id,
                            &review.reviewer,
                            role_id,
                            assignment_id,
                            as_of_ts,
                        );
                        let (eligible, reason_code, detail) = match eligibility {
                            Ok(()) => (true, None, None),
                            Err((code, detail)) => (false, Some(code), Some(detail)),
                        };
                        Some(RoleReviewEntryStatus {
                            reviewer: review.reviewer.clone(),
                            assignment_id: assignment_id.clone(),
                            verdict: review.verdict,
                            review_op_id: review.op_id.clone(),
                            eligible,
                            reason_code,
                            detail,
                        })
                    })
                    .collect::<Vec<_>>();
                reviews.sort_by(|left, right| left.reviewer.cmp(&right.reviewer));
                let eligible_approval_count = reviews
                    .iter()
                    .filter(|review| review.eligible && review.verdict == ReviewVerdict::Approve)
                    .count() as u32;
                let eligible_block_count = reviews
                    .iter()
                    .filter(|review| review.eligible && review.verdict == ReviewVerdict::Block)
                    .count() as u32;
                RoleReviewRequirementStatus {
                    role_id: requirement.role_id.clone(),
                    role_name: self
                        .roles
                        .get(&requirement.role_id)
                        .map(|role| role.name.clone()),
                    definition_op_id: requirement.definition_op_id.clone(),
                    required_approvals: requirement.required_approvals,
                    eligible_approval_count,
                    eligible_block_count,
                    satisfied: eligible_approval_count >= requirement.required_approvals
                        && eligible_block_count == 0,
                    reviews,
                }
            })
            .collect::<Vec<_>>();
        let satisfied = named.iter().all(|status| status.satisfied)
            && roles.iter().all(|status| status.satisfied);
        Some(CandidateReviewStatus {
            as_of_ts: as_of_ts.to_string(),
            named,
            roles,
            satisfied,
        })
    }

    pub fn candidate_reservations(&self, candidate_id: &str) -> Vec<&ReservationState> {
        self.reservations
            .values()
            .filter(|reservation| reservation.entity == candidate_id)
            .collect()
    }

    pub fn claim_disposition(&self, bead: &Bead, now_ts: &str) -> LeaseDisposition {
        let Some(claim) = bead.claim.as_ref() else {
            return LeaseDisposition::Closed;
        };
        if !claim.is_live(now_ts) {
            LeaseDisposition::Expired
        } else if bead.is_deleted() || bead.status == Status::Closed {
            LeaseDisposition::Orphaned
        } else {
            LeaseDisposition::Active
        }
    }

    /// Deterministically explain whether the named candidate may be landed.
    /// `lander` is supplied by the landing command to enforce named grants;
    /// read-only status views may omit it. This compatibility entry point uses
    /// the latest replayed operation timestamp. Time-aware surfaces should call
    /// `candidate_landability_at` with one explicit snapshot timestamp.
    pub fn candidate_landability(&self, candidate_id: &str, lander: Option<&str>) -> Landability {
        let as_of_ts = self
            .history
            .values()
            .flatten()
            .chain(self.orphan_history.iter())
            .map(|entry| entry.ts.as_str())
            .max()
            .unwrap_or("1970-01-01T00:00:00.000000Z");
        self.candidate_landability_at(candidate_id, lander, as_of_ts)
    }

    pub fn candidate_landability_at(
        &self,
        candidate_id: &str,
        lander: Option<&str>,
        as_of_ts: &str,
    ) -> Landability {
        let Some(candidate) = self.candidates.get(candidate_id) else {
            return Landability::from_reasons(vec![candidate_reason(
                LandabilityReasonCode::CandidateMissing,
                Some(candidate_id),
                "candidate does not exist",
            )]);
        };
        let mut reasons = Vec::new();

        if candidate.phase != CandidatePhase::Pending {
            reasons.push(candidate_reason(
                LandabilityReasonCode::PhaseNotPending,
                Some(candidate_id),
                format!("phase is {}", candidate.phase.as_str()),
            ));
        }

        let review_status = self
            .candidate_review_status(candidate_id, as_of_ts)
            .expect("known candidate has review status");
        for status in &review_status.named {
            match status.verdict {
                Some(ReviewVerdict::Approve) => {}
                Some(verdict) => reasons.push(candidate_reason(
                    LandabilityReasonCode::ReviewBlocking,
                    Some(&status.reviewer),
                    format!("latest verdict is {}", verdict.as_str()),
                )),
                None => reasons.push(candidate_reason(
                    LandabilityReasonCode::ReviewMissing,
                    Some(&status.reviewer),
                    "required reviewer has not approved",
                )),
            }
        }
        for requirement in &review_status.roles {
            let role_available = self.roles.get(&requirement.role_id).is_some_and(|role| {
                role.definition_op_id == requirement.definition_op_id
                    && role
                        .retired
                        .as_ref()
                        .is_none_or(|retirement| retirement.ts.as_str() > as_of_ts)
            });
            if !role_available {
                reasons.push(candidate_reason(
                    LandabilityReasonCode::ReviewRoleUnavailable,
                    Some(&requirement.role_id),
                    "required role is missing, retired, or does not match the immutable definition",
                ));
            }
            for review in &requirement.reviews {
                if review.eligible && review.verdict == ReviewVerdict::Block {
                    reasons.push(candidate_reason(
                        LandabilityReasonCode::ReviewRoleBlocking,
                        Some(format!("{}:{}", requirement.role_id, review.reviewer)),
                        format!(
                            "eligible role reviewer {} currently blocks",
                            review.reviewer
                        ),
                    ));
                } else if !review.eligible && review.verdict == ReviewVerdict::Approve {
                    reasons.push(candidate_reason(
                        LandabilityReasonCode::ReviewRoleApprovalIneligible,
                        Some(format!("{}:{}", requirement.role_id, review.reviewer)),
                        review.detail.as_deref().unwrap_or(
                            "recorded approval no longer has an eligible role assignment",
                        ),
                    ));
                }
            }
            if requirement.eligible_approval_count < requirement.required_approvals {
                reasons.push(candidate_reason(
                    LandabilityReasonCode::ReviewRoleQuorumMissing,
                    Some(&requirement.role_id),
                    format!(
                        "requires {} distinct eligible approval(s), has {}",
                        requirement.required_approvals, requirement.eligible_approval_count
                    ),
                ));
            }
        }

        for requirement in &candidate.evidence_requirements {
            for producer in &requirement.producers {
                match candidate
                    .evidence
                    .get(&(requirement.name.clone(), producer.clone()))
                {
                    Some(receipt) if receipt.outcome == EvidenceOutcome::Pass => {}
                    Some(receipt) => reasons.push(candidate_reason(
                        if matches!(
                            receipt.outcome,
                            EvidenceOutcome::Unavailable | EvidenceOutcome::Ambiguous
                        ) {
                            LandabilityReasonCode::EvidenceUnavailable
                        } else {
                            LandabilityReasonCode::EvidenceFailed
                        },
                        Some(format!("{}:{producer}", requirement.name)),
                        format!("latest outcome is {}", receipt.outcome.as_str()),
                    )),
                    None => reasons.push(candidate_reason(
                        LandabilityReasonCode::EvidenceMissing,
                        Some(format!("{}:{producer}", requirement.name)),
                        "required evidence is absent",
                    )),
                }
            }
        }

        let ancestry = candidate
            .evidence
            .values()
            .filter(|e| e.name == GIT_ANCESTRY_EVIDENCE)
            .max_by(|a, b| a.op_id.cmp(&b.op_id));
        match ancestry {
            Some(receipt) if receipt.outcome == EvidenceOutcome::Pass => {
                match receipt.payload.git_ancestry() {
                    Some(git)
                        if (git.repository_id == candidate.repository_id
                            || git.repository_id == candidate.landing_repository_id)
                            && git.object_format == candidate.object_format
                            && git.commit_oid == candidate.commit_oid
                            && git.base_oid == candidate.base_oid
                            && git.parent_oids == candidate.parent_oids =>
                    {
                        if git.base_is_ancestor != Some(true) {
                            reasons.push(candidate_reason(
                                LandabilityReasonCode::BaseNotAncestor,
                                Some(candidate_id),
                                "proposal base is not a verified ancestor",
                            ));
                        }
                        for other in self.candidates.values().filter(|other| {
                            other.candidate_id != candidate.candidate_id
                                && other.landing_repository_id == candidate.landing_repository_id
                                && other.object_format == candidate.object_format
                        }) {
                            match self.resolve_candidate_pair(candidate, other) {
                                CandidatePairResolution::Stale(detail) => {
                                    reasons.push(candidate_reason(
                                        LandabilityReasonCode::GitEvidenceStale,
                                        Some(&other.candidate_id),
                                        detail,
                                    ));
                                }
                                CandidatePairResolution::Ambiguous(detail) => {
                                    reasons.push(candidate_reason(
                                        LandabilityReasonCode::AncestorAmbiguous,
                                        Some(&other.candidate_id),
                                        detail,
                                    ));
                                }
                                CandidatePairResolution::Resolved { fact, .. } => {
                                    if fact.other_to_candidate_tip == GitRelationKind::Ancestor {
                                        self.check_ancestor_candidate(
                                            candidate,
                                            &other.candidate_id,
                                            Some(fact.other_to_candidate_base),
                                            &mut reasons,
                                        );
                                    }
                                }
                            }
                        }

                        // Preserve the v1 fail-closed treatment of rows that claim
                        // an unknown ancestor. Known candidates are handled by the
                        // exact pair resolver above.
                        for relation in git.candidate_relations.iter().filter(|relation| {
                            !self.candidates.contains_key(&relation.candidate_id)
                        }) {
                            if matches!(
                                relation.relation,
                                GitRelationKind::Ambiguous | GitRelationKind::Unavailable
                            ) || relation.base_relation.is_none()
                            {
                                reasons.push(candidate_reason(
                                    LandabilityReasonCode::AncestorAmbiguous,
                                    Some(&relation.candidate_id),
                                    "ancestry receipt contains an unresolved unknown candidate",
                                ));
                            } else if relation.relation == GitRelationKind::Ancestor {
                                reasons.push(candidate_reason(
                                    LandabilityReasonCode::AncestorMissing,
                                    Some(&relation.candidate_id),
                                    "ancestry receipt references an unknown candidate",
                                ));
                            }
                        }
                    }
                    Some(git) => reasons.push(candidate_reason(
                        if git.repository_id != candidate.repository_id
                            && git.repository_id != candidate.landing_repository_id
                        {
                            LandabilityReasonCode::RepositoryMismatch
                        } else {
                            LandabilityReasonCode::ProposalAnchorMismatch
                        },
                        Some(candidate_id),
                        "receipt does not match immutable proposal anchors",
                    )),
                    None => reasons.push(candidate_reason(
                        LandabilityReasonCode::ProposalAnchorMismatch,
                        Some(candidate_id),
                        "git-ancestry evidence has the wrong payload kind",
                    )),
                }
            }
            Some(receipt) => reasons.push(candidate_reason(
                if matches!(
                    receipt.outcome,
                    EvidenceOutcome::Unavailable | EvidenceOutcome::Ambiguous
                ) {
                    LandabilityReasonCode::GitEvidenceUnavailable
                } else {
                    LandabilityReasonCode::EvidenceFailed
                },
                Some(candidate_id),
                "latest git-ancestry receipt did not pass",
            )),
            None => reasons.push(candidate_reason(
                LandabilityReasonCode::GitEvidenceMissing,
                Some(candidate_id),
                "git-ancestry receipt is mandatory",
            )),
        }

        if candidate.object_availability_required {
            let availability = candidate
                .evidence
                .values()
                .filter(|evidence| {
                    evidence.name == crate::candidate::GIT_OBJECT_AVAILABILITY_EVIDENCE
                        && matches!(
                            &evidence.payload,
                            CandidateEvidencePayload::GitObjectAvailability(git)
                                if git.repository_id == candidate.landing_repository_id
                                    && git.object_format == candidate.object_format
                                    && git.candidate_oid == candidate.commit_oid
                        )
                })
                .max_by(|left, right| left.op_id.cmp(&right.op_id));
            let exact_reachability_pass = candidate.evidence.values().any(|evidence| {
                evidence.outcome == EvidenceOutcome::Pass
                    && match &evidence.payload {
                        CandidateEvidencePayload::GitLanding(git) => {
                            git.repository_id == candidate.landing_repository_id
                                && git.object_format == candidate.object_format
                                && git.candidate_oid == candidate.commit_oid
                                && git.candidate_reachable == Some(true)
                        }
                        CandidateEvidencePayload::GitReachability(git) => {
                            git.repository_id == candidate.landing_repository_id
                                && git.object_format == candidate.object_format
                                && git.candidate_oid == candidate.commit_oid
                                && git.candidate_reachable == Some(true)
                        }
                        _ => false,
                    }
            });
            match availability {
                Some(receipt)
                    if receipt.outcome == EvidenceOutcome::Pass
                        && matches!(
                            &receipt.payload,
                            CandidateEvidencePayload::GitObjectAvailability(git)
                                if git.object_available == Some(true)
                                    && git.observed_parent_oids == candidate.parent_oids
                        ) => {}
                Some(receipt)
                    if !exact_reachability_pass
                        && (receipt.outcome == EvidenceOutcome::Fail
                            || matches!(
                                &receipt.payload,
                                CandidateEvidencePayload::GitObjectAvailability(git)
                                    if git.object_available == Some(false)
                            )) =>
                {
                    let detail = match &receipt.payload {
                        CandidateEvidencePayload::GitObjectAvailability(git) => git
                            .detail
                            .clone()
                            .unwrap_or_else(|| "candidate commit object is not readable".into()),
                        _ => "candidate commit object is not readable".into(),
                    };
                    reasons.push(candidate_reason(
                        LandabilityReasonCode::ObjectUnreachable,
                        Some(&candidate.landing_repository_id),
                        detail,
                    ));
                }
                Some(receipt) if !exact_reachability_pass => {
                    let detail = match &receipt.payload {
                        CandidateEvidencePayload::GitObjectAvailability(git) => git
                            .detail
                            .clone()
                            .unwrap_or_else(|| {
                                format!(
                                    "latest object-availability outcome is {}",
                                    receipt.outcome.as_str()
                                )
                            }),
                        _ => "object-availability evidence has the wrong payload".into(),
                    };
                    reasons.push(candidate_reason(
                        LandabilityReasonCode::ObjectAvailabilityUnavailable,
                        Some(&candidate.landing_repository_id),
                        detail,
                    ));
                }
                None if !exact_reachability_pass => reasons.push(candidate_reason(
                    LandabilityReasonCode::ObjectAvailabilityMissing,
                    Some(&candidate.landing_repository_id),
                    "no current receipt proves the candidate object is readable from the bound landing repository",
                )),
                Some(_) | None => {}
            }
        }

        match &candidate.authorization {
            Some(auth)
                if matches!(
                    auth.status,
                    AuthorizationStatus::Granted | AuthorizationStatus::Conditional
                ) =>
            {
                if let Some(lander) = lander {
                    if !auth.grantees.iter().any(|grantee| grantee == lander) {
                        reasons.push(candidate_reason(
                            LandabilityReasonCode::ActorNotGrantee,
                            Some(lander),
                            "actor is not a named landing grantee",
                        ));
                    }
                }
                for condition in &auth.conditions {
                    if !candidate
                        .evidence
                        .values()
                        .any(|e| e.name == *condition && e.outcome == EvidenceOutcome::Pass)
                    {
                        reasons.push(candidate_reason(
                            LandabilityReasonCode::ConditionUnsatisfied,
                            Some(condition),
                            "no current passing receipt satisfies this condition",
                        ));
                    }
                }
            }
            Some(auth) => reasons.push(candidate_reason(
                if auth.status == AuthorizationStatus::Revoked {
                    LandabilityReasonCode::AuthorizationRevoked
                } else {
                    LandabilityReasonCode::AuthorizationAbsent
                },
                Some(&auth.op_id),
                format!("authorization is {}", auth.status.as_str()),
            )),
            None => reasons.push(candidate_reason(
                LandabilityReasonCode::AuthorizationAbsent,
                Some(candidate_id),
                "candidate has no landing authorization",
            )),
        }

        Landability::from_reasons(reasons)
    }

    pub fn candidate_policy_snapshot(&self, candidate_id: &str) -> Option<CandidatePolicySnapshot> {
        let as_of_ts = self
            .history
            .values()
            .flatten()
            .chain(self.orphan_history.iter())
            .map(|entry| entry.ts.as_str())
            .max()
            .unwrap_or("1970-01-01T00:00:00.000000Z");
        self.candidate_policy_snapshot_at(candidate_id, as_of_ts)
    }

    pub fn candidate_policy_snapshot_at(
        &self,
        candidate_id: &str,
        as_of_ts: &str,
    ) -> Option<CandidatePolicySnapshot> {
        let candidate = self.candidates.get(candidate_id)?;
        let mut review_op_ids: Vec<String> = candidate
            .reviews
            .values()
            .map(|review| review.op_id.clone())
            .collect();
        review_op_ids.sort();
        review_op_ids.dedup();

        let mut evidence_op_ids: Vec<String> = candidate
            .evidence
            .values()
            .map(|evidence| evidence.op_id.clone())
            .collect();
        evidence_op_ids.sort();
        evidence_op_ids.dedup();

        let mut pair_evidence_op_ids: Vec<String> = self
            .candidate_pair_evidence
            .values()
            .filter(|evidence| {
                evidence.subject_candidate_id == candidate_id
                    || evidence.known_candidate_id == candidate_id
            })
            .map(|evidence| evidence.evidence_op_id.clone())
            .collect();
        pair_evidence_op_ids.sort();
        pair_evidence_op_ids.dedup();

        Some(CandidatePolicySnapshot {
            phase_op_id: candidate.phase_op_id.clone(),
            review_policy_op_id: Some(candidate.review_policy_op_id.clone()),
            landing_repository_op_id: Some(candidate.landing_repository_op_id.clone()),
            review_op_ids,
            evidence_op_ids,
            pair_evidence_op_ids,
            authorization_op_id: candidate
                .authorization
                .as_ref()
                .map(|authorization| authorization.op_id.clone()),
            authorization_status: candidate
                .authorization
                .as_ref()
                .map(|authorization| authorization.status),
            pre_transition_landability: self.candidate_landability_at(candidate_id, None, as_of_ts),
        })
    }

    fn resolve_candidate_pair(
        &self,
        candidate: &CandidateRecord,
        other: &CandidateRecord,
    ) -> CandidatePairResolution {
        let observations = [
            self.pair_source_observation(candidate, other, false),
            self.pair_source_observation(other, candidate, true),
        ];

        let conflicts: Vec<String> = observations
            .iter()
            .filter_map(|observation| match observation {
                PairSourceObservation::Conflict(detail) => Some(detail.clone()),
                _ => None,
            })
            .collect();
        if !conflicts.is_empty() {
            return CandidatePairResolution::Ambiguous(conflicts.join("; "));
        }

        let determinate: Vec<(CandidatePairFact, String, String)> = observations
            .iter()
            .filter_map(|observation| match observation {
                PairSourceObservation::Determinate {
                    fact,
                    source,
                    evidence_op_id,
                } => Some((*fact, source.clone(), evidence_op_id.clone())),
                _ => None,
            })
            .collect();
        if let Some((first, first_source, _)) = determinate.first() {
            if let Some((_, conflicting_source, _)) =
                determinate.iter().find(|(fact, _, _)| fact != first)
            {
                return CandidatePairResolution::Ambiguous(format!(
                    "conflicting determinate pair observations from {first_source} and {conflicting_source}"
                ));
            }
            let mut evidence_op_ids: Vec<String> = determinate
                .iter()
                .map(|(_, _, evidence_op_id)| evidence_op_id.clone())
                .collect();
            evidence_op_ids.sort();
            evidence_op_ids.dedup();
            return CandidatePairResolution::Resolved {
                fact: *first,
                evidence_op_ids,
            };
        }

        let unknown: Vec<String> = observations
            .iter()
            .filter_map(|observation| match observation {
                PairSourceObservation::Unknown(detail) => Some(detail.clone()),
                _ => None,
            })
            .collect();
        if !unknown.is_empty() {
            return CandidatePairResolution::Ambiguous(unknown.join("; "));
        }

        let absent: Vec<String> = observations
            .iter()
            .filter_map(|observation| match observation {
                PairSourceObservation::Absent(detail) => Some(detail.clone()),
                _ => None,
            })
            .collect();
        CandidatePairResolution::Stale(format!(
            "no exact pair observation covers proposal op {}; {}",
            other.proposal_op_id,
            absent.join("; ")
        ))
    }

    pub fn candidate_containment_basis(
        &self,
        predecessor_id: &str,
        successor_id: &str,
    ) -> Option<CandidateContainmentBasis> {
        let predecessor = self.candidates.get(predecessor_id)?;
        let successor = self.candidates.get(successor_id)?;
        if predecessor_id == successor_id
            || predecessor.store_id != successor.store_id
            || predecessor.landing_repository_id != successor.landing_repository_id
            || predecessor.object_format != successor.object_format
            || predecessor.entity != successor.entity
        {
            return None;
        }
        match self.resolve_candidate_pair(successor, predecessor) {
            CandidatePairResolution::Resolved {
                fact,
                evidence_op_ids,
            } if fact.other_to_candidate_tip == GitRelationKind::Ancestor => {
                Some(CandidateContainmentBasis {
                    predecessor_candidate_id: predecessor_id.to_string(),
                    successor_candidate_id: successor_id.to_string(),
                    predecessor_to_successor_base: fact.other_to_candidate_base,
                    predecessor_to_successor_tip: fact.other_to_candidate_tip,
                    evidence_op_ids,
                })
            }
            _ => None,
        }
    }

    fn pair_source_observation(
        &self,
        subject: &CandidateRecord,
        known: &CandidateRecord,
        reciprocal: bool,
    ) -> PairSourceObservation {
        let source_direction = if reciprocal { "reciprocal" } else { "direct" };
        let Some(record) = self
            .candidate_pair_evidence
            .get(&(subject.candidate_id.clone(), known.candidate_id.clone()))
        else {
            return PairSourceObservation::Absent(self.missing_pair_source_detail(
                subject,
                known,
                source_direction,
            ));
        };
        let source = format!(
            "{} evidence op {}",
            subject.candidate_id, record.evidence_op_id
        );

        if record.subject_candidate_id != subject.candidate_id
            || record.subject_proposal_op_id != subject.proposal_op_id
            || record.subject_store_id != subject.store_id
            || (record.repository_id != subject.repository_id
                && record.repository_id != subject.landing_repository_id)
            || record.object_format != subject.object_format
            || record.subject_commit_oid != subject.commit_oid
            || record.subject_base_oid != subject.base_oid
            || record.known_candidate_id != known.candidate_id
        {
            return PairSourceObservation::Conflict(format!(
                "{source} does not match immutable subject anchors"
            ));
        }

        let schema_v2 = record.relation_schema >= GIT_RELATION_SCHEMA_V2;
        if reciprocal && !schema_v2 {
            return PairSourceObservation::Absent(format!(
                "{source} is legacy and has no reciprocal relation"
            ));
        }

        if schema_v2 {
            let Some(snapshot) = &record.producer_snapshot else {
                return PairSourceObservation::Unknown(format!(
                    "{source} uses relation schema {} without producer snapshot provenance",
                    record.relation_schema
                ));
            };
            if snapshot.store_id != subject.store_id {
                return PairSourceObservation::Conflict(format!(
                    "{source} snapshot store {} does not match candidate store {}",
                    snapshot.store_id, subject.store_id
                ));
            }
            if !snapshot
                .observed_candidates
                .contains(&(known.candidate_id.clone(), known.proposal_op_id.clone()))
            {
                return PairSourceObservation::Absent(snapshot_omission_detail(
                    &source,
                    snapshot,
                    &known.proposal_op_id,
                ));
            }
        }

        if !record
            .covered_candidates
            .contains(&(known.candidate_id.clone(), known.proposal_op_id.clone()))
        {
            return PairSourceObservation::Absent(format!(
                "{source} does not declare coverage of proposal op {}",
                known.proposal_op_id
            ));
        }

        let exact_rows: Vec<&GitCandidateRelation> = record
            .relations
            .iter()
            .filter(|relation| {
                relation.candidate_id == known.candidate_id
                    && relation.proposal_op_id == known.proposal_op_id
                    && relation.commit_oid == known.commit_oid
                    && (!schema_v2 || relation.base_oid.as_deref() == Some(&known.base_oid))
            })
            .collect();
        if exact_rows.is_empty() {
            return PairSourceObservation::Absent(format!(
                "{source} has no row matching the exact commit and base anchors for proposal op {}",
                known.proposal_op_id
            ));
        }
        if exact_rows.len() != 1 {
            return PairSourceObservation::Conflict(format!(
                "{source} contains {} exact rows for one candidate pair",
                exact_rows.len()
            ));
        }

        let relation = exact_rows[0];
        let (base, tip) = if reciprocal {
            (
                relation.subject_to_known_base,
                relation.subject_to_known_tip,
            )
        } else {
            (relation.base_relation, Some(relation.relation))
        };
        let (Some(base), Some(tip)) = (base, tip) else {
            return PairSourceObservation::Unknown(format!(
                "{source} has a partial {source_direction} relation"
            ));
        };
        if matches!(
            base,
            GitRelationKind::Unavailable | GitRelationKind::Ambiguous
        ) || matches!(
            tip,
            GitRelationKind::Unavailable | GitRelationKind::Ambiguous
        ) {
            return PairSourceObservation::Unknown(format!(
                "{source} base relation is {} and tip relation is {}",
                base.as_str(),
                tip.as_str()
            ));
        }
        if base == GitRelationKind::Ancestor && tip == GitRelationKind::NotAncestor {
            return PairSourceObservation::Conflict(format!(
                "{source} says the commit is an ancestor of the base but not the tip"
            ));
        }

        PairSourceObservation::Determinate {
            fact: CandidatePairFact {
                other_to_candidate_base: base,
                other_to_candidate_tip: tip,
            },
            source,
            evidence_op_id: record.evidence_op_id.clone(),
        }
    }

    fn missing_pair_source_detail(
        &self,
        subject: &CandidateRecord,
        known: &CandidateRecord,
        source_direction: &str,
    ) -> String {
        let Some(receipt) = subject
            .evidence
            .values()
            .filter(|evidence| evidence.name == GIT_ANCESTRY_EVIDENCE)
            .max_by(|left, right| left.op_id.cmp(&right.op_id))
        else {
            return format!(
                "no {source_direction} git-ancestry receipt exists for {}",
                subject.candidate_id
            );
        };
        let source = format!(
            "{source_direction} source {} evidence op {}",
            subject.candidate_id, receipt.op_id
        );
        if receipt.op_id < known.proposal_op_id {
            return format!(
                "{source} predates proposal op {}; refresh from a store snapshot containing both exact proposal ops",
                known.proposal_op_id
            );
        }

        match receipt.payload.git_ancestry() {
            Some(git) => match &git.producer_snapshot {
                Some(snapshot)
                    if !snapshot
                        .observed_candidates
                        .contains(&(known.candidate_id.clone(), known.proposal_op_id.clone())) =>
                {
                    snapshot_omission_detail(&source, snapshot, &known.proposal_op_id)
                }
                Some(snapshot) => format!(
                    "{source} declares proposal op {} in {}, but contains no exact pair row; refresh from a store snapshot containing both exact proposal ops",
                    known.proposal_op_id,
                    snapshot_summary(snapshot)
                ),
                None => format!(
                    "{source} is a legacy receipt without producer snapshot provenance, so the target cannot distinguish incomplete input from malformed coverage; refresh with a v2 binary from a store snapshot containing both exact proposal ops"
                ),
            },
            None => format!(
                "{source} has the wrong payload kind; refresh from a store snapshot containing both exact proposal ops"
            ),
        }
    }

    fn check_ancestor_candidate(
        &self,
        candidate: &CandidateRecord,
        ancestor_id: &str,
        base_relation: Option<GitRelationKind>,
        reasons: &mut Vec<LandabilityReason>,
    ) {
        let Some(ancestor) = self.candidates.get(ancestor_id) else {
            reasons.push(candidate_reason(
                LandabilityReasonCode::AncestorMissing,
                Some(ancestor_id),
                "ancestry receipt references an unknown candidate",
            ));
            return;
        };
        match ancestor.phase {
            CandidatePhase::Landed | CandidatePhase::LandedOutOfBand => {}
            CandidatePhase::Superseded => {
                let mut cursor = ancestor;
                let mut visited = BTreeSet::new();
                while cursor.phase == CandidatePhase::Superseded {
                    if !visited.insert(cursor.candidate_id.clone()) {
                        reasons.push(candidate_reason(
                            LandabilityReasonCode::SupersessionCycle,
                            Some(ancestor_id),
                            "supersession chain contains a cycle",
                        ));
                        return;
                    }
                    let Some(next_id) = cursor.successor_id.as_deref() else {
                        reasons.push(candidate_reason(
                            LandabilityReasonCode::SupersessionBroken,
                            Some(&cursor.candidate_id),
                            "superseded candidate has no successor",
                        ));
                        return;
                    };
                    let Some(next) = self.candidates.get(next_id) else {
                        reasons.push(candidate_reason(
                            LandabilityReasonCode::SupersessionBroken,
                            Some(next_id),
                            "successor candidate does not exist",
                        ));
                        return;
                    };
                    cursor = next;
                }
                if cursor.candidate_id != candidate.candidate_id
                    && !matches!(
                        cursor.phase,
                        CandidatePhase::Landed | CandidatePhase::LandedOutOfBand
                    )
                {
                    reasons.push(candidate_reason(
                        LandabilityReasonCode::AncestorSupersessionUnresolved,
                        Some(ancestor_id),
                        format!(
                            "chain ends at {} ({})",
                            cursor.candidate_id,
                            cursor.phase.as_str()
                        ),
                    ));
                }
            }
            CandidatePhase::Pending => {
                let blocking_review = ancestor
                    .reviews
                    .values()
                    .any(|review| review.verdict == ReviewVerdict::Block);
                let revoked = ancestor
                    .authorization
                    .as_ref()
                    .is_some_and(|authorization| {
                        authorization.status == AuthorizationStatus::Revoked
                    });
                reasons.push(candidate_reason(
                    if blocking_review {
                        LandabilityReasonCode::AncestorBlocked
                    } else if revoked {
                        LandabilityReasonCode::AncestorAuthorizationRevoked
                    } else {
                        LandabilityReasonCode::AncestorPending
                    },
                    Some(ancestor_id),
                    "an ancestor candidate is not safely resolved",
                ));
            }
            CandidatePhase::Abandoned => match base_relation {
                Some(GitRelationKind::Ancestor) => {}
                Some(GitRelationKind::NotAncestor) => reasons.push(candidate_reason(
                    LandabilityReasonCode::AncestorAbandoned,
                    Some(ancestor_id),
                    "an abandoned candidate was introduced after the immutable base",
                )),
                Some(GitRelationKind::Unavailable | GitRelationKind::Ambiguous) | None => {
                    // The caller records ancestor_ambiguous. Do not misclassify an
                    // unproven base relation as introduced-after-base.
                }
            },
        }
    }

    /// Iterate over non-deleted beads.
    pub fn live_beads(&self) -> impl Iterator<Item = &Bead> {
        self.beads.values().filter(|b| !b.is_deleted())
    }

    /// A bead is "ready" when:
    ///   - it's not deleted
    ///   - status is `open`
    ///   - every blocking parent dep is either missing, deleted, or closed
    ///
    /// Claim filtering (PRD: "not currently claimed by another actor") arrives
    /// in M4 once `claim`/`release` ops are wired through the reducer.
    pub fn is_ready(&self, bead: &Bead) -> bool {
        if bead.is_deleted() || bead.status != Status::Open {
            return false;
        }
        bead.deps.iter().all(|(parent_id, _)| {
            self.beads
                .get(parent_id)
                .is_none_or(|p| p.is_deleted() || p.status == Status::Closed)
        })
    }

    /// Non-blocking relation children of `parent_id`, in bead id order.
    pub fn relation_children_of<'a>(&'a self, parent_id: &str) -> Vec<(&'a Bead, &'a str)> {
        let mut children: Vec<(&Bead, &str)> = self
            .live_beads()
            .flat_map(|b| {
                b.rels.iter().filter_map(move |(p, k)| {
                    if p == parent_id {
                        Some((b, k.as_str()))
                    } else {
                        None
                    }
                })
            })
            .collect();
        children.sort_by(|(a, ak), (b, bk)| a.id.cmp(&b.id).then_with(|| ak.cmp(bk)));
        children
    }

    /// Blocking dependency children of `parent_id`, in bead id order.
    pub fn dependency_children_of<'a>(&'a self, parent_id: &str) -> Vec<(&'a Bead, &'a str)> {
        let mut children: Vec<(&Bead, &str)> = self
            .live_beads()
            .flat_map(|b| {
                b.deps.iter().filter_map(move |(p, k)| {
                    if p == parent_id {
                        Some((b, k.as_str()))
                    } else {
                        None
                    }
                })
            })
            .collect();
        children.sort_by(|(a, ak), (b, bk)| a.id.cmp(&b.id).then_with(|| ak.cmp(bk)));
        children
    }

    /// Iterate ready beads, in BTreeMap (id) order.
    pub fn ready_beads(&self) -> impl Iterator<Item = &Bead> {
        self.beads.values().filter(move |b| self.is_ready(b))
    }

    /// Filtering variant of `ready_beads` that also rejects beads currently
    /// claimed by another actor (with a non-expired lease as-of `now_ts`).
    pub fn ready_beads_for<'a>(
        &'a self,
        actor: &'a str,
        now_ts: &'a str,
    ) -> impl Iterator<Item = &'a Bead> {
        self.beads.values().filter(move |b| {
            if !self.is_ready(b) {
                return false;
            }
            !matches!(&b.claim, Some(c) if c.is_live(now_ts) && c.claimed_by != actor)
        })
    }

    /// All un-acked messages whose recipient is `actor`, in send-order.
    pub fn inbox_for<'a>(&'a self, actor: &str) -> Vec<&'a MsgRecord> {
        let mut msgs: Vec<&MsgRecord> = self
            .messages
            .values()
            .filter(|m| m.to == actor && m.ack_op_id.is_none())
            .collect();
        msgs.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        msgs
    }

    /// Root request messages involving `actor`, in send-order.
    pub fn requests_for<'a>(&'a self, actor: &str) -> Vec<&'a MsgRecord> {
        let mut requests: Vec<&MsgRecord> = self
            .messages
            .values()
            .filter(|m| m.request_state.is_some() && (m.from == actor || m.to == actor))
            .collect();
        requests.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        requests
    }

    /// Every message exchanged between `actor` and `peer`, in send-order.
    ///
    /// Neither `inbox_for` nor `requests_for` can reconstruct a two-sided
    /// thread: the first is inbound and unacked-only, the second is limited to
    /// request roots, so a plain `note` or `fyi` this actor sent appears in no
    /// listing at all. This fold answers "what have these two said to each
    /// other", in both directions and regardless of ack or request state.
    pub fn conversation_between<'a>(&'a self, actor: &str, peer: &str) -> Vec<&'a MsgRecord> {
        let mut msgs: Vec<&MsgRecord> = self
            .messages
            .values()
            .filter(|m| (m.from == actor && m.to == peer) || (m.from == peer && m.to == actor))
            .collect();
        msgs.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        msgs
    }

    /// Accepted message carrying a sender-scoped idempotency key.
    pub fn message_by_idempotency<'a>(&'a self, actor: &str, key: &str) -> Option<&'a MsgRecord> {
        self.messages
            .values()
            .find(|m| m.from == actor && m.idempotency_key.as_deref() == Some(key))
    }

    pub fn board_post_by_idempotency<'a>(
        &'a self,
        actor: &str,
        key: &str,
    ) -> Option<&'a BoardPostRecord> {
        self.board_posts
            .values()
            .find(|post| post.from == actor && post.idempotency_key.as_deref() == Some(key))
    }

    pub fn board_decisions_for_topic<'a>(&'a self, topic: &str) -> Vec<&'a BoardDecisionRecord> {
        let mut decisions = self
            .board_decisions
            .values()
            .filter(|decision| decision.topic == topic)
            .collect::<Vec<_>>();
        decisions.sort_by(|left, right| left.op_id.cmp(&right.op_id));
        decisions
    }

    pub fn board_questions_for_topic<'a>(&'a self, topic: &str) -> Vec<&'a DecisionQuestionRecord> {
        let mut questions = self
            .board_questions
            .values()
            .filter(|question| question.topic == topic)
            .collect::<Vec<_>>();
        questions.sort_by(|left, right| {
            left.opened_op_id
                .cmp(&right.opened_op_id)
                .then_with(|| left.position.cmp(&right.position))
        });
        questions
    }

    pub fn board_question_counts(&self, topic: Option<&str>) -> DecisionQuestionCounts {
        let mut counts = DecisionQuestionCounts::default();
        for question in self
            .board_questions
            .values()
            .filter(|question| topic.is_none_or(|topic| question.topic == topic))
        {
            counts.total += 1;
            match question.status {
                DecisionQuestionStatus::Open => counts.open += 1,
                DecisionQuestionStatus::Deferred => counts.deferred += 1,
                DecisionQuestionStatus::Superseded => counts.superseded += 1,
                DecisionQuestionStatus::Closed => counts.closed += 1,
            }
        }
        counts.unresolved = counts.open + counts.deferred;
        counts
    }

    pub fn board_questions_for_decision<'a>(
        &'a self,
        decision_id: &str,
    ) -> Vec<&'a DecisionQuestionRecord> {
        let mut questions = self
            .board_questions
            .values()
            .filter(|question| question.decision_id == decision_id)
            .collect::<Vec<_>>();
        questions.sort_by(|left, right| left.position.cmp(&right.position));
        questions
    }

    pub fn board_question_transition_by_idempotency<'a>(
        &'a self,
        actor: &str,
        key: &str,
    ) -> Option<(
        &'a DecisionQuestionRecord,
        &'a DecisionQuestionTransitionRecord,
    )> {
        self.board_questions.values().find_map(|question| {
            question
                .transitions
                .iter()
                .find(|transition| {
                    transition.actor == actor && transition.idempotency_key.as_deref() == Some(key)
                })
                .map(|transition| (question, transition))
        })
    }

    pub fn watched_topics_for(&self, actor: &str) -> Vec<String> {
        self.board_topic_watches
            .values()
            .filter(|watch| watch.actor == actor && watch.watching)
            .map(|watch| watch.topic.clone())
            .collect()
    }

    pub fn topic_watchers(&self, topic: &str) -> Vec<String> {
        self.board_topic_watches
            .values()
            .filter(|watch| watch.topic == topic && watch.watching)
            .map(|watch| watch.actor.clone())
            .collect()
    }

    /// All discussion-board posts, optionally filtered by topic, in send-order.
    pub fn board_posts_for<'a>(&'a self, topic: Option<&str>) -> Vec<&'a BoardPostRecord> {
        let mut posts: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|p| topic.is_none_or(|t| p.topic == t))
            .collect();
        posts.sort_by(|a, b| {
            b.sticky
                .cmp(&a.sticky)
                .then_with(|| a.sent_op_id.cmp(&b.sent_op_id))
        });
        posts
    }

    /// Public discussion-board posts newer than `actor`'s read cursor.
    /// The caller's own posts are omitted so agents can poll for new external
    /// messages without being notified about their own publication.
    pub fn unread_board_posts_for<'a>(
        &'a self,
        actor: &str,
        topic: Option<&str>,
    ) -> Vec<&'a BoardPostRecord> {
        let mut posts: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|p| {
                let cursor = if let Some(topic) = topic {
                    self.discussion_cursor_for(actor, Some(topic))
                } else {
                    self.discussion_cursor_for(actor, Some(&p.topic))
                }
                .map(String::as_str)
                .unwrap_or("");
                p.from != actor
                    && p.sent_op_id.as_str() > cursor
                    && topic.is_none_or(|t| p.topic == t)
            })
            .collect();
        posts.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        posts
    }

    /// Unread public posts routed explicitly to this actor, either because the
    /// actor watched the topic when the post was accepted or was named by the
    /// publisher. The ordinary discussion cursor consumes both views.
    pub fn unread_board_notifications_for<'a>(
        &'a self,
        actor: &str,
        topic: Option<&str>,
    ) -> Vec<&'a BoardPostRecord> {
        let mut posts: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|post| {
                let cursor = self
                    .discussion_cursor_for(actor, Some(&post.topic))
                    .map(String::as_str)
                    .unwrap_or("");
                post.from != actor
                    && post
                        .notification_recipients
                        .iter()
                        .any(|name| name == actor)
                    && post.sent_op_id.as_str() > cursor
                    && topic.is_none_or(|wanted| post.topic == wanted)
            })
            .collect();
        posts.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        posts
    }

    pub fn discussion_cursor_for(&self, actor: &str, topic: Option<&str>) -> Option<&String> {
        let global = self.board_read_cursors.get(actor);
        let Some(topic) = topic else {
            return global;
        };
        let topic = self
            .board_topic_read_cursors
            .get(&(actor.to_string(), topic.to_string()));
        match (topic, global) {
            (Some(topic), Some(global)) => {
                if topic.as_str() >= global.as_str() {
                    Some(topic)
                } else {
                    Some(global)
                }
            }
            (Some(topic), None) => Some(topic),
            (None, Some(global)) => Some(global),
            (None, None) => None,
        }
    }

    /// Direct replies to a discussion-board post, in send-order.
    pub fn replies_to<'a>(&'a self, post_id: &str) -> Vec<&'a BoardPostRecord> {
        let mut posts: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|p| p.reply_to.as_deref() == Some(post_id))
            .collect();
        posts.sort_by(|a, b| {
            b.sticky
                .cmp(&a.sticky)
                .then_with(|| a.sent_op_id.cmp(&b.sent_op_id))
        });
        posts
    }

    /// Discussion targets that have been declared actionable but carry no bead,
    /// in send-order for posts and topic order for topics. This is the state
    /// behind "which discussions still need tracker action?".
    pub fn unrouted_posts<'a>(&'a self, topic: Option<&str>) -> Vec<&'a BoardPostRecord> {
        let mut posts: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|p| p.route.needs_action() && topic.is_none_or(|t| p.topic == t))
            .collect();
        posts.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        posts
    }

    pub fn unrouted_topics<'a>(&'a self, topic: Option<&str>) -> Vec<&'a BoardTopicRecord> {
        self.board_topics
            .values()
            .filter(|t| t.route.needs_action() && topic.is_none_or(|wanted| t.topic == wanted))
            .collect()
    }

    /// Discussion posts and topics linked to `bead_id`, so a bead can be traced
    /// back to the thread that produced it.
    pub fn discussion_sources_for<'a>(
        &'a self,
        bead_id: &str,
    ) -> (Vec<&'a BoardPostRecord>, Vec<&'a BoardTopicRecord>) {
        let mut posts: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|p| p.route.issues.contains(bead_id))
            .collect();
        posts.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
        let topics: Vec<&BoardTopicRecord> = self
            .board_topics
            .values()
            .filter(|t| t.route.issues.contains(bead_id))
            .collect();
        (posts, topics)
    }

    /// Live session leases as-of `now_ts`, in start order.
    pub fn live_sessions(&self, now_ts: &str) -> Vec<&SessionRecord> {
        let mut sessions: Vec<&SessionRecord> = self
            .sessions
            .values()
            .filter(|s| s.is_live(now_ts))
            .collect();
        sessions.sort_by(|a, b| {
            a.actor
                .cmp(&b.actor)
                .then_with(|| a.started_op_id.cmp(&b.started_op_id))
        });
        sessions
    }

    /// Live session leases held under `actor`. More than one means concurrent
    /// sessions are sharing a single identity.
    pub fn live_sessions_for(&self, actor: &str, now_ts: &str) -> Vec<&SessionRecord> {
        self.live_sessions(now_ts)
            .into_iter()
            .filter(|s| s.actor == actor)
            .collect()
    }

    /// Discussion topics ordered by current activity, newest first.
    pub fn board_topics_by_activity(&self) -> Vec<&BoardTopicRecord> {
        let mut topics: Vec<&BoardTopicRecord> = self.board_topics.values().collect();
        topics.sort_by(|a, b| {
            b.last_activity_op_id
                .cmp(&a.last_activity_op_id)
                .then_with(|| a.topic.cmp(&b.topic))
        });
        topics
    }

    /// Descendant replies to a root post in parent-before-child order.
    pub fn thread_posts<'a>(&'a self, root_post_id: &str) -> Vec<(usize, &'a BoardPostRecord)> {
        let mut out = Vec::new();
        let Some(root) = self.board_posts.get(root_post_id) else {
            return out;
        };
        out.push((0, root));
        self.collect_thread_children(root_post_id, 1, &mut out);
        out
    }

    fn collect_thread_children<'a>(
        &'a self,
        post_id: &str,
        depth: usize,
        out: &mut Vec<(usize, &'a BoardPostRecord)>,
    ) {
        let mut children: Vec<&BoardPostRecord> = self
            .board_posts
            .values()
            .filter(|p| p.reply_to.as_deref() == Some(post_id))
            .collect();
        children.sort_by(|a, b| {
            b.sticky
                .cmp(&a.sticky)
                .then_with(|| a.sent_op_id.cmp(&b.sent_op_id))
        });
        for child in children {
            out.push((depth, child));
            self.collect_thread_children(&child.post_id, depth + 1, out);
        }
    }

    /// Returns the rejection reason from the most recent history entry of
    /// `op_id`, or `None` if the op was accepted (or not found).
    pub fn rejection_reason(&self, op_id: &str) -> Option<String> {
        for entries in self.history.values() {
            for e in entries {
                if e.op_id == op_id {
                    return if e.accepted { None } else { e.reason.clone() };
                }
            }
        }
        for e in &self.orphan_history {
            if e.op_id == op_id {
                return if e.accepted { None } else { e.reason.clone() };
            }
        }
        None
    }

    /// `true` if `op_id` was found in history and was accepted.
    pub fn was_accepted(&self, op_id: &str) -> bool {
        for entries in self.history.values() {
            for e in entries {
                if e.op_id == op_id {
                    return e.accepted;
                }
            }
        }
        for e in &self.orphan_history {
            if e.op_id == op_id {
                return e.accepted;
            }
        }
        false
    }
}
