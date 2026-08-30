//! Derived state from replaying ops.
//!
//! All state lives in memory; we never persist `state.rs` data structures to
//! disk in v0.2 (snapshots are deferred). The reducer in `crate::reducer`
//! mutates this struct in filename order.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Serialize, Serializer};

use crate::candidate::{
    AuthorizationStatus, CandidateEvidencePayload, CandidatePhase, EvidenceOutcome,
    EvidenceRequirement, GIT_ANCESTRY_EVIDENCE, GitRelationKind, Landability, LandabilityReason,
    ReviewVerdict,
};
use crate::op::Status;

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
    /// Candidate protocol state, derived exclusively from candidate ops.
    pub candidates: BTreeMap<String, CandidateRecord>,
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
    pub repository_id: String,
    pub object_format: String,
    pub commit_oid: String,
    pub base_oid: String,
    pub parent_oids: Vec<String>,
    pub paths: Vec<String>,
    pub authorizer: String,
    pub reviewers: Vec<String>,
    pub evidence_requirements: Vec<EvidenceRequirement>,
    pub evidence_refs: Vec<String>,
    pub phase: CandidatePhase,
    pub phase_op_id: String,
    pub successor_id: Option<String>,
    pub reviews: BTreeMap<String, CandidateReviewRecord>,
    #[serde(serialize_with = "serialize_candidate_evidence")]
    pub evidence: BTreeMap<(String, String), CandidateEvidenceRecord>,
    pub authorization: Option<CandidateAuthorizationRecord>,
    pub landed: Option<CandidateLandedRecord>,
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
    pub verdict: ReviewVerdict,
    pub body: Option<String>,
    pub evidence_refs: Vec<String>,
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

#[derive(Debug, Clone)]
pub struct CandidateIdempotencyRecord {
    pub candidate_id: String,
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
    code: &str,
    subject: Option<impl Into<String>>,
    detail: impl Into<String>,
) -> LandabilityReason {
    LandabilityReason {
        code: code.to_string(),
        subject: subject.map(Into::into),
        detail: detail.into(),
    }
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
    /// read-only status views may omit it.
    pub fn candidate_landability(&self, candidate_id: &str, lander: Option<&str>) -> Landability {
        let Some(candidate) = self.candidates.get(candidate_id) else {
            return Landability::from_reasons(vec![candidate_reason(
                "candidate_missing",
                Some(candidate_id),
                "candidate does not exist",
            )]);
        };
        let mut reasons = Vec::new();

        if candidate.phase != CandidatePhase::Pending {
            reasons.push(candidate_reason(
                "phase_not_pending",
                Some(candidate_id),
                format!("phase is {}", candidate.phase.as_str()),
            ));
        }

        for reviewer in &candidate.reviewers {
            match candidate.reviews.get(reviewer) {
                Some(review) if review.verdict == ReviewVerdict::Approve => {}
                Some(review) => reasons.push(candidate_reason(
                    "review_blocking",
                    Some(reviewer),
                    format!("latest verdict is {}", review.verdict.as_str()),
                )),
                None => reasons.push(candidate_reason(
                    "review_missing",
                    Some(reviewer),
                    "required reviewer has not approved",
                )),
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
                            "evidence_unavailable"
                        } else {
                            "evidence_failed"
                        },
                        Some(format!("{}:{producer}", requirement.name)),
                        format!("latest outcome is {}", receipt.outcome.as_str()),
                    )),
                    None => reasons.push(candidate_reason(
                        "evidence_missing",
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
            Some(receipt) if receipt.outcome == EvidenceOutcome::Pass => match &receipt.payload {
                CandidateEvidencePayload::GitAncestry(git)
                    if git.repository_id == candidate.repository_id
                        && git.object_format == candidate.object_format
                        && git.commit_oid == candidate.commit_oid
                        && git.base_oid == candidate.base_oid
                        && git.parent_oids == candidate.parent_oids =>
                {
                    if git.base_is_ancestor != Some(true) {
                        reasons.push(candidate_reason(
                            "base_not_ancestor",
                            Some(candidate_id),
                            "proposal base is not a verified ancestor",
                        ));
                    }
                    let expected: BTreeSet<(String, String)> = self
                        .candidates
                        .values()
                        .filter(|other| {
                            other.candidate_id != candidate.candidate_id
                                && other.repository_id == candidate.repository_id
                        })
                        .map(|other| (other.candidate_id.clone(), other.proposal_op_id.clone()))
                        .collect();
                    let covered: BTreeSet<(String, String)> =
                        git.covered_candidates.iter().cloned().collect();
                    for missing in expected.difference(&covered) {
                        reasons.push(candidate_reason(
                            "git_evidence_stale",
                            Some(&missing.0),
                            format!("proposal op {} is not covered", missing.1),
                        ));
                    }
                    for relation in &git.candidate_relations {
                        let tip_ambiguous = matches!(
                            relation.relation,
                            GitRelationKind::Ambiguous | GitRelationKind::Unavailable
                        );
                        let base_ambiguous = matches!(
                            relation.base_relation,
                            None | Some(GitRelationKind::Ambiguous | GitRelationKind::Unavailable)
                        );
                        let inconsistent = relation.base_relation
                            == Some(GitRelationKind::Ancestor)
                            && relation.relation == GitRelationKind::NotAncestor;
                        if tip_ambiguous || base_ambiguous || inconsistent {
                            let base = relation
                                .base_relation
                                .map(GitRelationKind::as_str)
                                .unwrap_or("missing");
                            reasons.push(candidate_reason(
                                "ancestor_ambiguous",
                                Some(&relation.candidate_id),
                                format!(
                                    "base relation is {base}; tip relation is {}",
                                    relation.relation.as_str()
                                ),
                            ));
                        }
                        if relation.relation == GitRelationKind::Ancestor {
                            self.check_ancestor_candidate(
                                candidate,
                                &relation.candidate_id,
                                relation.base_relation,
                                &mut reasons,
                            );
                        }
                    }
                }
                CandidateEvidencePayload::GitAncestry(git) => reasons.push(candidate_reason(
                    if git.repository_id != candidate.repository_id {
                        "repository_mismatch"
                    } else {
                        "proposal_anchor_mismatch"
                    },
                    Some(candidate_id),
                    "receipt does not match immutable proposal anchors",
                )),
                _ => reasons.push(candidate_reason(
                    "proposal_anchor_mismatch",
                    Some(candidate_id),
                    "git-ancestry evidence has the wrong payload kind",
                )),
            },
            Some(receipt) => reasons.push(candidate_reason(
                if matches!(
                    receipt.outcome,
                    EvidenceOutcome::Unavailable | EvidenceOutcome::Ambiguous
                ) {
                    "git_evidence_unavailable"
                } else {
                    "evidence_failed"
                },
                Some(candidate_id),
                "latest git-ancestry receipt did not pass",
            )),
            None => reasons.push(candidate_reason(
                "git_evidence_missing",
                Some(candidate_id),
                "git-ancestry receipt is mandatory",
            )),
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
                            "actor_not_grantee",
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
                            "condition_unsatisfied",
                            Some(condition),
                            "no current passing receipt satisfies this condition",
                        ));
                    }
                }
            }
            Some(auth) => reasons.push(candidate_reason(
                if auth.status == AuthorizationStatus::Revoked {
                    "authorization_revoked"
                } else {
                    "authorization_absent"
                },
                Some(&auth.op_id),
                format!("authorization is {}", auth.status.as_str()),
            )),
            None => reasons.push(candidate_reason(
                "authorization_absent",
                Some(candidate_id),
                "candidate has no landing authorization",
            )),
        }

        Landability::from_reasons(reasons)
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
                "ancestor_missing",
                Some(ancestor_id),
                "ancestry receipt references an unknown candidate",
            ));
            return;
        };
        match ancestor.phase {
            CandidatePhase::Landed => {}
            CandidatePhase::Superseded => {
                let mut cursor = ancestor;
                let mut visited = BTreeSet::new();
                while cursor.phase == CandidatePhase::Superseded {
                    if !visited.insert(cursor.candidate_id.clone()) {
                        reasons.push(candidate_reason(
                            "supersession_cycle",
                            Some(ancestor_id),
                            "supersession chain contains a cycle",
                        ));
                        return;
                    }
                    let Some(next_id) = cursor.successor_id.as_deref() else {
                        reasons.push(candidate_reason(
                            "supersession_broken",
                            Some(&cursor.candidate_id),
                            "superseded candidate has no successor",
                        ));
                        return;
                    };
                    let Some(next) = self.candidates.get(next_id) else {
                        reasons.push(candidate_reason(
                            "supersession_broken",
                            Some(next_id),
                            "successor candidate does not exist",
                        ));
                        return;
                    };
                    cursor = next;
                }
                if cursor.candidate_id != candidate.candidate_id
                    && cursor.phase != CandidatePhase::Landed
                {
                    reasons.push(candidate_reason(
                        "ancestor_supersession_unresolved",
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
                        "ancestor_blocked"
                    } else if revoked {
                        "ancestor_authorization_revoked"
                    } else {
                        "ancestor_pending"
                    },
                    Some(ancestor_id),
                    "an ancestor candidate is not safely resolved",
                ));
            }
            CandidatePhase::Abandoned => match base_relation {
                Some(GitRelationKind::Ancestor) => {}
                Some(GitRelationKind::NotAncestor) => reasons.push(candidate_reason(
                    "ancestor_abandoned",
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
