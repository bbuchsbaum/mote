//! Typed op envelope and per-kind structs.
//!
//! `Op` is an internally-tagged enum (`tag = "kind"`); each variant is a
//! newtype around a struct so the envelope fields (`v`, `op`, `ts`, `actor`)
//! sit at the top level of the JSON, alongside `kind` and the kind-specific
//! payload. Serializing `Op::Create(CreateOp { v, op, ts, actor, entity, set })`
//! yields:
//!
//! ```json
//! {"v":1,"op":"<id>","ts":"...","actor":"...","kind":"create","entity":"bd-...","set":{...}}
//! ```
//!
//! Note: serde's "internally-tagged enum" pattern requires newtype-around-struct;
//! tuple variants with multiple fields and naked `flatten` against a tagged enum
//! are not supported. The shape below is the supported form.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::candidate::{
    AuthorizationStatus, CandidateEvidencePayload, EvidenceOutcome, EvidenceRequirement,
    ReviewVerdict,
};
use crate::ids;

fn is_false(value: &bool) -> bool {
    !*value
}

pub type BeadId = String;
pub type OpId = String;
pub type FieldName = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Doing,
    Blocked,
    Review,
    Closed,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Doing => "doing",
            Status::Blocked => "blocked",
            Status::Review => "review",
            Status::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Status::Open),
            "doing" => Some(Status::Doing),
            "blocked" => Some(Status::Blocked),
            "review" => Some(Status::Review),
            "closed" => Some(Status::Closed),
            _ => None,
        }
    }
}

/// Scalar fields a bead carries. Each is optional in patches and creates;
/// presence indicates "this op writes this field".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScalarSet {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
}

impl ScalarSet {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.status.is_none()
            && self.priority.is_none()
            && self.body.is_none()
            && self.assignee.is_none()
    }

    /// Iterate (field_name, _) over fields that are `Some`. The names match
    /// the per-field clock keys used in derived state.
    pub fn fields(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.title.is_some() {
            out.push("title");
        }
        if self.status.is_some() {
            out.push("status");
        }
        if self.priority.is_some() {
            out.push("priority");
        }
        if self.body.is_some() {
            out.push("body");
        }
        if self.assignee.is_some() {
            out.push("assignee");
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    pub set: ScalarSet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatchOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expect: BTreeMap<FieldName, OpId>,
    pub set: ScalarSet,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    pub tag: String,
}

/// Dep edge op. Note: the field carrying the edge kind is named `dep_kind`
/// (not `kind`) to avoid colliding with the envelope's `kind` tag in the
/// internally-tagged `Op` enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId, // child
    pub parent: BeadId,
    #[serde(default = "default_dep_kind")]
    pub dep_kind: String,
}

fn default_dep_kind() -> String {
    "blocks".to_string()
}

/// Non-blocking relationship op. These edges express hierarchy/containment
/// and are intentionally ignored by readiness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId, // child
    pub parent: BeadId,
    #[serde(default = "default_rel_kind")]
    pub rel_kind: String,
}

fn default_rel_kind() -> String {
    "parent".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    pub note_kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expect: BTreeMap<FieldName, OpId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    pub to: String,
    pub ttl_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_claim: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: BeadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_claim: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsgSendOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub msg_id: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<BeadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation: Option<String>,
    pub msg_kind: String,
    pub body: String,
    /// Parent request for a structured response or decline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Stable request conversation identifier. Request roots default to their
    /// own `msg_id`; replies inherit the root's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Sender-scoped retry key. Accepted keys are unique per actor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Open request ids atomically answered by this message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    /// Reject unless the recipient has a valid session lease at `ts`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsgAckOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub msg_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MsgResolveOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub msg_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardPostOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub post_id: String,
    pub topic: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// `post` (default), `decision`, or `summary`. A `summary` post becomes the
    /// topic's pinned current-state pointer; a `decision` post is counted so
    /// readers can find the thread's conclusions without re-reading it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_kind: Option<String>,
    /// Open request ids atomically answered by this public post.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answers: Vec<String>,
    /// Explicit actor recipients for public-post attention routing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify: Vec<String>,
    /// Author-scoped retry key for the complete post content and routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardWatchOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub topic: String,
    pub watching: bool,
}

/// Routing op: link a discussion post or topic to tracker work, or declare
/// that it still needs (or no longer needs) tracker action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardRouteOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    /// Exactly one of `post_id` / `topic` identifies the routing target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// One of `VALID_ROUTE_STATES`.
    pub route_state: String,
    /// Bead this discussion routes to. Required for `routed`, forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<BeadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// TTL-bounded session lease. Distinguishes concurrent sessions that would
/// otherwise be indistinguishable behind one persisted actor identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub session_id: String,
    pub ttl_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Publishing process id, recorded so `mote doctor` can tell one long
    /// session from several concurrent ones sharing an identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Explicit renewal of one existing session lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeartbeatOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub session_id: String,
    pub ttl_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Session-scoped declared availability/work state. This is intentionally not
/// actor-global: two concurrent sessions for one actor may declare different
/// intents without racing through a last-writer-wins register.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatusOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub session_id: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<BeadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEndOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardReadOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub upto_op_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Reject rather than silently no-op if replay has already advanced past
    /// this boundary. Absent on legacy/head-marking operations.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTopicOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardStickyOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub post_id: String,
    pub sticky: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardSupersedeOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub old_post_id: String,
    pub new_post_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardRetractOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub post_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveOpenOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub reservation_id: String,
    pub entity: BeadId,
    pub paths: Vec<String>,
    pub ttl_s: u32,
    #[serde(default = "default_reserve_mode")]
    pub mode: String,
}

fn default_reserve_mode() -> String {
    "exclusive".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveCloseOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub reservation_id: String,
    /// `None` (or empty) means close all paths in the referenced reservation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

/// Re-home a still-live reservation whose bound work has become terminal.
/// The adopter must already hold a live claim on `entity`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveAdoptOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub reservation_id: String,
    pub entity: BeadId,
    pub expect_reservation: String,
    pub ttl_s: u32,
}

/// Immutable proposal record. Git names have already been resolved to full
/// object ids by the publishing CLI; replay never consults the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateProposeOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub entity: BeadId,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateEvidenceOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub candidate_oid: String,
    pub evidence_id: String,
    pub name: String,
    pub evidence_kind: String,
    pub producer_tool: String,
    pub outcome: EvidenceOutcome,
    pub payload: CandidateEvidencePayload,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateReviewOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub verdict: ReviewVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_review: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateAuthorizeOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub status: AuthorizationStatus,
    pub grantees: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_authorization: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRevokeOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub expect_authorization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateSupersedeOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub successor_id: String,
    pub expect_phase: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateAbandonOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub expect_phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateLandedOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub candidate_id: String,
    pub evidence_id: String,
    pub expect_phase: String,
    pub expect_authorization: String,
    pub target_ref: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    Create(CreateOp),
    Patch(PatchOp),
    TagAdd(TagOp),
    TagRemove(TagOp),
    DepAdd(DepOp),
    DepRemove(DepOp),
    RelAdd(RelOp),
    RelRemove(RelOp),
    Note(NoteOp),
    Close(CloseOp),
    Delete(DeleteOp),
    Claim(ClaimOp),
    Release(ReleaseOp),
    MsgSend(MsgSendOp),
    MsgAck(MsgAckOp),
    MsgResolve(MsgResolveOp),
    BoardPost(BoardPostOp),
    BoardRead(BoardReadOp),
    BoardWatch(BoardWatchOp),
    BoardTopic(BoardTopicOp),
    BoardSticky(BoardStickyOp),
    BoardSupersede(BoardSupersedeOp),
    BoardRetract(BoardRetractOp),
    BoardRoute(BoardRouteOp),
    SessionStart(SessionStartOp),
    SessionHeartbeat(SessionHeartbeatOp),
    SessionStatus(SessionStatusOp),
    SessionEnd(SessionEndOp),
    ReserveOpen(ReserveOpenOp),
    ReserveClose(ReserveCloseOp),
    ReserveAdopt(ReserveAdoptOp),
    CandidatePropose(CandidateProposeOp),
    CandidateEvidence(CandidateEvidenceOp),
    CandidateReview(CandidateReviewOp),
    CandidateAuthorize(CandidateAuthorizeOp),
    CandidateRevoke(CandidateRevokeOp),
    CandidateSupersede(CandidateSupersedeOp),
    CandidateAbandon(CandidateAbandonOp),
    CandidateLanded(CandidateLandedOp),
}

impl Op {
    pub fn op_id(&self) -> &str {
        match self {
            Op::Create(o) => &o.op,
            Op::Patch(o) => &o.op,
            Op::TagAdd(o) | Op::TagRemove(o) => &o.op,
            Op::DepAdd(o) | Op::DepRemove(o) => &o.op,
            Op::RelAdd(o) | Op::RelRemove(o) => &o.op,
            Op::Note(o) => &o.op,
            Op::Close(o) => &o.op,
            Op::Delete(o) => &o.op,
            Op::Claim(o) => &o.op,
            Op::Release(o) => &o.op,
            Op::MsgSend(o) => &o.op,
            Op::MsgAck(o) => &o.op,
            Op::MsgResolve(o) => &o.op,
            Op::BoardPost(o) => &o.op,
            Op::BoardRead(o) => &o.op,
            Op::BoardWatch(o) => &o.op,
            Op::BoardTopic(o) => &o.op,
            Op::BoardSticky(o) => &o.op,
            Op::BoardSupersede(o) => &o.op,
            Op::BoardRetract(o) => &o.op,
            Op::BoardRoute(o) => &o.op,
            Op::SessionStart(o) => &o.op,
            Op::SessionHeartbeat(o) => &o.op,
            Op::SessionStatus(o) => &o.op,
            Op::SessionEnd(o) => &o.op,
            Op::ReserveOpen(o) => &o.op,
            Op::ReserveClose(o) => &o.op,
            Op::ReserveAdopt(o) => &o.op,
            Op::CandidatePropose(o) => &o.op,
            Op::CandidateEvidence(o) => &o.op,
            Op::CandidateReview(o) => &o.op,
            Op::CandidateAuthorize(o) => &o.op,
            Op::CandidateRevoke(o) => &o.op,
            Op::CandidateSupersede(o) => &o.op,
            Op::CandidateAbandon(o) => &o.op,
            Op::CandidateLanded(o) => &o.op,
        }
    }

    pub fn actor(&self) -> &str {
        match self {
            Op::Create(o) => &o.actor,
            Op::Patch(o) => &o.actor,
            Op::TagAdd(o) | Op::TagRemove(o) => &o.actor,
            Op::DepAdd(o) | Op::DepRemove(o) => &o.actor,
            Op::RelAdd(o) | Op::RelRemove(o) => &o.actor,
            Op::Note(o) => &o.actor,
            Op::Close(o) => &o.actor,
            Op::Delete(o) => &o.actor,
            Op::Claim(o) => &o.actor,
            Op::Release(o) => &o.actor,
            Op::MsgSend(o) => &o.actor,
            Op::MsgAck(o) => &o.actor,
            Op::MsgResolve(o) => &o.actor,
            Op::BoardPost(o) => &o.actor,
            Op::BoardRead(o) => &o.actor,
            Op::BoardWatch(o) => &o.actor,
            Op::BoardTopic(o) => &o.actor,
            Op::BoardSticky(o) => &o.actor,
            Op::BoardSupersede(o) => &o.actor,
            Op::BoardRetract(o) => &o.actor,
            Op::BoardRoute(o) => &o.actor,
            Op::SessionStart(o) => &o.actor,
            Op::SessionHeartbeat(o) => &o.actor,
            Op::SessionStatus(o) => &o.actor,
            Op::SessionEnd(o) => &o.actor,
            Op::ReserveOpen(o) => &o.actor,
            Op::ReserveClose(o) => &o.actor,
            Op::ReserveAdopt(o) => &o.actor,
            Op::CandidatePropose(o) => &o.actor,
            Op::CandidateEvidence(o) => &o.actor,
            Op::CandidateReview(o) => &o.actor,
            Op::CandidateAuthorize(o) => &o.actor,
            Op::CandidateRevoke(o) => &o.actor,
            Op::CandidateSupersede(o) => &o.actor,
            Op::CandidateAbandon(o) => &o.actor,
            Op::CandidateLanded(o) => &o.actor,
        }
    }

    pub fn ts(&self) -> &str {
        match self {
            Op::Create(o) => &o.ts,
            Op::Patch(o) => &o.ts,
            Op::TagAdd(o) | Op::TagRemove(o) => &o.ts,
            Op::DepAdd(o) | Op::DepRemove(o) => &o.ts,
            Op::RelAdd(o) | Op::RelRemove(o) => &o.ts,
            Op::Note(o) => &o.ts,
            Op::Close(o) => &o.ts,
            Op::Delete(o) => &o.ts,
            Op::Claim(o) => &o.ts,
            Op::Release(o) => &o.ts,
            Op::MsgSend(o) => &o.ts,
            Op::MsgAck(o) => &o.ts,
            Op::MsgResolve(o) => &o.ts,
            Op::BoardPost(o) => &o.ts,
            Op::BoardRead(o) => &o.ts,
            Op::BoardWatch(o) => &o.ts,
            Op::BoardTopic(o) => &o.ts,
            Op::BoardSticky(o) => &o.ts,
            Op::BoardSupersede(o) => &o.ts,
            Op::BoardRetract(o) => &o.ts,
            Op::BoardRoute(o) => &o.ts,
            Op::SessionStart(o) => &o.ts,
            Op::SessionHeartbeat(o) => &o.ts,
            Op::SessionStatus(o) => &o.ts,
            Op::SessionEnd(o) => &o.ts,
            Op::ReserveOpen(o) => &o.ts,
            Op::ReserveClose(o) => &o.ts,
            Op::ReserveAdopt(o) => &o.ts,
            Op::CandidatePropose(o) => &o.ts,
            Op::CandidateEvidence(o) => &o.ts,
            Op::CandidateReview(o) => &o.ts,
            Op::CandidateAuthorize(o) => &o.ts,
            Op::CandidateRevoke(o) => &o.ts,
            Op::CandidateSupersede(o) => &o.ts,
            Op::CandidateAbandon(o) => &o.ts,
            Op::CandidateLanded(o) => &o.ts,
        }
    }

    /// Returns the entity (bead id) this op acts on, or `None` for op kinds
    /// that are not entity-scoped (`msg_ack`, `msg_resolve`, `reserve_close`,
    /// the discussion ops, and the session ops; `msg_send` may optionally
    /// reference an entity). `board_route` names a bead but acts on the
    /// discussion target, so its history stays in the discussion plane.
    pub fn entity(&self) -> Option<&str> {
        match self {
            Op::Create(o) => Some(&o.entity),
            Op::Patch(o) => Some(&o.entity),
            Op::TagAdd(o) | Op::TagRemove(o) => Some(&o.entity),
            Op::DepAdd(o) | Op::DepRemove(o) => Some(&o.entity),
            Op::RelAdd(o) | Op::RelRemove(o) => Some(&o.entity),
            Op::Note(o) => Some(&o.entity),
            Op::Close(o) => Some(&o.entity),
            Op::Delete(o) => Some(&o.entity),
            Op::Claim(o) => Some(&o.entity),
            Op::Release(o) => Some(&o.entity),
            Op::MsgSend(o) => o.entity.as_deref(),
            Op::MsgAck(_) | Op::MsgResolve(_) => None,
            Op::BoardPost(_) => None,
            Op::BoardRead(_) | Op::BoardWatch(_) => None,
            Op::BoardTopic(_) => None,
            Op::BoardSticky(_) => None,
            Op::BoardSupersede(_) | Op::BoardRetract(_) => None,
            Op::BoardRoute(_) => None,
            Op::SessionStart(_)
            | Op::SessionHeartbeat(_)
            | Op::SessionStatus(_)
            | Op::SessionEnd(_) => None,
            Op::ReserveOpen(o) => Some(&o.entity),
            Op::ReserveClose(_) => None,
            Op::ReserveAdopt(o) => Some(&o.entity),
            Op::CandidatePropose(o) => Some(&o.candidate_id),
            Op::CandidateEvidence(o) => Some(&o.candidate_id),
            Op::CandidateReview(o) => Some(&o.candidate_id),
            Op::CandidateAuthorize(o) => Some(&o.candidate_id),
            Op::CandidateRevoke(o) => Some(&o.candidate_id),
            Op::CandidateSupersede(o) => Some(&o.candidate_id),
            Op::CandidateAbandon(o) => Some(&o.candidate_id),
            Op::CandidateLanded(o) => Some(&o.candidate_id),
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Op::Create(_) => "create",
            Op::Patch(_) => "patch",
            Op::TagAdd(_) => "tag_add",
            Op::TagRemove(_) => "tag_remove",
            Op::DepAdd(_) => "dep_add",
            Op::DepRemove(_) => "dep_remove",
            Op::RelAdd(_) => "rel_add",
            Op::RelRemove(_) => "rel_remove",
            Op::Note(_) => "note",
            Op::Close(_) => "close",
            Op::Delete(_) => "delete",
            Op::Claim(_) => "claim",
            Op::Release(_) => "release",
            Op::MsgSend(_) => "msg_send",
            Op::MsgAck(_) => "msg_ack",
            Op::MsgResolve(_) => "msg_resolve",
            Op::BoardPost(_) => "board_post",
            Op::BoardRead(_) => "board_read",
            Op::BoardWatch(_) => "board_watch",
            Op::BoardTopic(_) => "board_topic",
            Op::BoardSticky(_) => "board_sticky",
            Op::BoardSupersede(_) => "board_supersede",
            Op::BoardRetract(_) => "board_retract",
            Op::BoardRoute(_) => "board_route",
            Op::SessionStart(_) => "session_start",
            Op::SessionHeartbeat(_) => "session_heartbeat",
            Op::SessionStatus(_) => "session_status",
            Op::SessionEnd(_) => "session_end",
            Op::ReserveOpen(_) => "reserve_open",
            Op::ReserveClose(_) => "reserve_close",
            Op::ReserveAdopt(_) => "reserve_adopt",
            Op::CandidatePropose(_) => "candidate_propose",
            Op::CandidateEvidence(_) => "candidate_evidence",
            Op::CandidateReview(_) => "candidate_review",
            Op::CandidateAuthorize(_) => "candidate_authorize",
            Op::CandidateRevoke(_) => "candidate_revoke",
            Op::CandidateSupersede(_) => "candidate_supersede",
            Op::CandidateAbandon(_) => "candidate_abandon",
            Op::CandidateLanded(_) => "candidate_landed",
        }
    }

    /// Candidate mutation retry identity, if this is a candidate operation.
    pub fn candidate_idempotency(&self) -> Option<(&str, &str)> {
        match self {
            Op::CandidatePropose(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateEvidence(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateReview(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateAuthorize(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateRevoke(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateSupersede(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateAbandon(o) => Some((&o.candidate_id, &o.idempotency_key)),
            Op::CandidateLanded(o) => Some((&o.candidate_id, &o.idempotency_key)),
            _ => None,
        }
    }
}

/// Helper: build a `CreateOp` with `op` blank, `v=1`, and `ts` formatted.
pub fn make_create(actor: String, entity: BeadId, set: ScalarSet, ts: jiff::Timestamp) -> Op {
    Op::Create(CreateOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        set,
    })
}

pub fn make_patch(
    actor: String,
    entity: BeadId,
    expect: BTreeMap<FieldName, OpId>,
    set: ScalarSet,
    ts: jiff::Timestamp,
) -> Op {
    Op::Patch(PatchOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        expect,
        set,
    })
}

pub fn make_tag(add: bool, actor: String, entity: BeadId, tag: String, ts: jiff::Timestamp) -> Op {
    let payload = TagOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        tag,
    };
    if add {
        Op::TagAdd(payload)
    } else {
        Op::TagRemove(payload)
    }
}

pub fn make_dep(
    add: bool,
    actor: String,
    entity: BeadId,
    parent: BeadId,
    dep_kind: String,
    ts: jiff::Timestamp,
) -> Op {
    let payload = DepOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        parent,
        dep_kind,
    };
    if add {
        Op::DepAdd(payload)
    } else {
        Op::DepRemove(payload)
    }
}

pub fn make_rel(
    add: bool,
    actor: String,
    entity: BeadId,
    parent: BeadId,
    rel_kind: String,
    ts: jiff::Timestamp,
) -> Op {
    let payload = RelOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        parent,
        rel_kind,
    };
    if add {
        Op::RelAdd(payload)
    } else {
        Op::RelRemove(payload)
    }
}

pub fn make_note(
    actor: String,
    entity: BeadId,
    note_kind: String,
    text: String,
    ts: jiff::Timestamp,
) -> Op {
    Op::Note(NoteOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        note_kind,
        text,
    })
}

pub fn make_close(
    actor: String,
    entity: BeadId,
    expect: BTreeMap<FieldName, OpId>,
    ts: jiff::Timestamp,
) -> Op {
    Op::Close(CloseOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        expect,
    })
}

pub fn make_delete(actor: String, entity: BeadId, ts: jiff::Timestamp) -> Op {
    Op::Delete(DeleteOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
    })
}

pub fn make_claim(
    actor: String,
    entity: BeadId,
    to: String,
    ttl_s: u32,
    expect_claim: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::Claim(ClaimOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        to,
        ttl_s,
        expect_claim,
    })
}

pub fn make_release(
    actor: String,
    entity: BeadId,
    expect_claim: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::Release(ReleaseOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        entity,
        expect_claim,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn make_msg_send(
    actor: String,
    msg_id: String,
    to: String,
    entity: Option<BeadId>,
    reservation: Option<String>,
    msg_kind: String,
    body: String,
    ts: jiff::Timestamp,
) -> Op {
    make_msg_send_with_metadata(
        actor,
        msg_id,
        to,
        entity,
        reservation,
        msg_kind,
        body,
        None,
        None,
        None,
        ts,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn make_msg_send_with_metadata(
    actor: String,
    msg_id: String,
    to: String,
    entity: Option<BeadId>,
    reservation: Option<String>,
    msg_kind: String,
    body: String,
    reply_to: Option<String>,
    correlation_id: Option<String>,
    idempotency_key: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_msg_send_with_metadata_and_answers(
        actor,
        msg_id,
        to,
        entity,
        reservation,
        msg_kind,
        body,
        reply_to,
        correlation_id,
        idempotency_key,
        Vec::new(),
        ts,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn make_msg_send_with_metadata_and_answers(
    actor: String,
    msg_id: String,
    to: String,
    entity: Option<BeadId>,
    reservation: Option<String>,
    msg_kind: String,
    body: String,
    reply_to: Option<String>,
    correlation_id: Option<String>,
    idempotency_key: Option<String>,
    answers: Vec<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_msg_send_with_options(
        actor,
        msg_id,
        to,
        entity,
        reservation,
        msg_kind,
        body,
        reply_to,
        correlation_id,
        idempotency_key,
        answers,
        false,
        ts,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn make_msg_send_with_options(
    actor: String,
    msg_id: String,
    to: String,
    entity: Option<BeadId>,
    reservation: Option<String>,
    msg_kind: String,
    body: String,
    reply_to: Option<String>,
    correlation_id: Option<String>,
    idempotency_key: Option<String>,
    answers: Vec<String>,
    require_live: bool,
    ts: jiff::Timestamp,
) -> Op {
    Op::MsgSend(MsgSendOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        msg_id,
        to,
        entity,
        reservation,
        msg_kind,
        body,
        reply_to,
        correlation_id,
        idempotency_key,
        answers,
        require_live,
    })
}

pub fn make_reserve_open(
    actor: String,
    reservation_id: String,
    entity: BeadId,
    paths: Vec<String>,
    ttl_s: u32,
    ts: jiff::Timestamp,
) -> Op {
    Op::ReserveOpen(ReserveOpenOp {
        v: 2,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        reservation_id,
        entity,
        paths,
        ttl_s,
        mode: "exclusive".to_string(),
    })
}

pub fn make_reserve_close(
    actor: String,
    reservation_id: String,
    paths: Option<Vec<String>>,
    ts: jiff::Timestamp,
) -> Op {
    Op::ReserveClose(ReserveCloseOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        reservation_id,
        paths,
    })
}

pub fn make_reserve_adopt(
    actor: String,
    reservation_id: String,
    entity: BeadId,
    expect_reservation: String,
    ttl_s: u32,
    ts: jiff::Timestamp,
) -> Op {
    Op::ReserveAdopt(ReserveAdoptOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        reservation_id,
        entity,
        expect_reservation,
        ttl_s,
    })
}

pub fn make_msg_ack(actor: String, msg_id: String, ts: jiff::Timestamp) -> Op {
    Op::MsgAck(MsgAckOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        msg_id,
    })
}

pub fn make_msg_resolve(actor: String, msg_id: String, ts: jiff::Timestamp) -> Op {
    Op::MsgResolve(MsgResolveOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        msg_id,
    })
}

pub fn make_board_post(
    actor: String,
    post_id: String,
    topic: String,
    body: String,
    reply_to: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_board_post_of_kind(actor, post_id, topic, body, reply_to, None, ts)
}

#[allow(clippy::too_many_arguments)]
pub fn make_board_post_of_kind(
    actor: String,
    post_id: String,
    topic: String,
    body: String,
    reply_to: Option<String>,
    post_kind: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_board_post_of_kind_with_answers(
        actor,
        post_id,
        topic,
        body,
        reply_to,
        post_kind,
        Vec::new(),
        ts,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn make_board_post_of_kind_with_answers(
    actor: String,
    post_id: String,
    topic: String,
    body: String,
    reply_to: Option<String>,
    post_kind: Option<String>,
    answers: Vec<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_board_post_with_options(
        actor,
        post_id,
        topic,
        body,
        reply_to,
        post_kind,
        answers,
        Vec::new(),
        None,
        ts,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn make_board_post_with_options(
    actor: String,
    post_id: String,
    topic: String,
    body: String,
    reply_to: Option<String>,
    post_kind: Option<String>,
    answers: Vec<String>,
    notify: Vec<String>,
    idempotency_key: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardPost(BoardPostOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        post_id,
        topic,
        body,
        reply_to,
        post_kind,
        answers,
        notify,
        idempotency_key,
    })
}

pub fn make_board_watch(actor: String, topic: String, watching: bool, ts: jiff::Timestamp) -> Op {
    Op::BoardWatch(BoardWatchOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        topic,
        watching,
    })
}

pub fn make_board_route(
    actor: String,
    post_id: Option<String>,
    topic: Option<String>,
    route_state: String,
    entity: Option<BeadId>,
    note: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardRoute(BoardRouteOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        post_id,
        topic,
        route_state,
        entity,
        note,
    })
}

pub fn make_session_start(
    actor: String,
    session_id: String,
    ttl_s: u32,
    label: Option<String>,
    pid: Option<u32>,
    ts: jiff::Timestamp,
) -> Op {
    Op::SessionStart(SessionStartOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        session_id,
        ttl_s,
        label,
        pid,
    })
}

pub fn make_session_heartbeat(
    actor: String,
    session_id: String,
    ttl_s: u32,
    idempotency_key: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::SessionHeartbeat(SessionHeartbeatOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        session_id,
        ttl_s,
        idempotency_key,
    })
}

pub fn make_session_status(
    actor: String,
    session_id: String,
    status: String,
    message: Option<String>,
    issue: Option<BeadId>,
    idempotency_key: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::SessionStatus(SessionStatusOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        session_id,
        status,
        message,
        issue,
        idempotency_key,
    })
}

pub fn make_session_end(actor: String, session_id: String, ts: jiff::Timestamp) -> Op {
    Op::SessionEnd(SessionEndOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        session_id,
    })
}

pub fn make_board_read(
    actor: String,
    upto_op_id: String,
    topic: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_board_read_with_policy(actor, upto_op_id, topic, false, ts)
}

pub fn make_board_read_through(
    actor: String,
    upto_op_id: String,
    topic: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    make_board_read_with_policy(actor, upto_op_id, topic, true, ts)
}

fn make_board_read_with_policy(
    actor: String,
    upto_op_id: String,
    topic: Option<String>,
    strict: bool,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardRead(BoardReadOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        upto_op_id,
        topic,
        strict,
    })
}

pub fn make_board_topic(
    actor: String,
    topic: String,
    title: Option<String>,
    body: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardTopic(BoardTopicOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        topic,
        title,
        body,
    })
}

pub fn make_board_sticky(actor: String, post_id: String, sticky: bool, ts: jiff::Timestamp) -> Op {
    Op::BoardSticky(BoardStickyOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        post_id,
        sticky,
    })
}

pub fn make_board_supersede(
    actor: String,
    old_post_id: String,
    new_post_id: String,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardSupersede(BoardSupersedeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        old_post_id,
        new_post_id,
    })
}

pub fn make_board_retract(
    actor: String,
    post_id: String,
    reason: String,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardRetract(BoardRetractOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        post_id,
        reason,
    })
}

pub const VALID_NOTE_KINDS: &[&str] = &[
    "note", "progress", "decision", "design", "handoff", "blocker",
];
pub const VALID_MSG_KINDS: &[&str] = &[
    "note", "request", "response", "decline", "handoff", "blocked", "fyi",
];
pub const VALID_REPLY_KINDS: &[&str] = &["response", "decline"];
pub const VALID_POST_KINDS: &[&str] = &["post", "decision", "summary"];
/// Routing states a discussion post or topic can carry. `open` is the implicit
/// default and is also writable, so a resolved thread can be reopened.
pub const VALID_ROUTE_STATES: &[&str] = &["open", "needs_bead", "routed", "resolved"];
pub const VALID_SESSION_INTENTS: &[&str] = &["available", "working", "waiting", "blocked", "away"];

pub fn validate_note_kind(k: &str) -> bool {
    VALID_NOTE_KINDS.contains(&k)
}

pub fn validate_post_kind(k: &str) -> bool {
    VALID_POST_KINDS.contains(&k)
}

pub fn validate_route_state(s: &str) -> bool {
    VALID_ROUTE_STATES.contains(&s)
}

pub fn validate_session_intent(s: &str) -> bool {
    VALID_SESSION_INTENTS.contains(&s)
}

pub fn validate_msg_kind(k: &str) -> bool {
    VALID_MSG_KINDS.contains(&k)
}

pub fn validate_idempotency_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().count() <= 128
        && key.trim() == key
        && !key.chars().any(char::is_control)
}

/// Stable digest of the semantic fields covered by a session retry key.
/// Envelope fields (`op`, `ts`) are excluded so a transport retry at a later
/// timestamp resolves to the originally accepted action.
pub fn session_action_digest(op: &Op) -> Option<String> {
    let semantic = match op {
        Op::SessionHeartbeat(o) => serde_json::json!({
            "kind": "session_heartbeat",
            "session_id": o.session_id,
            "ttl_s": o.ttl_s,
        }),
        Op::SessionStatus(o) => serde_json::json!({
            "kind": "session_status",
            "session_id": o.session_id,
            "status": o.status,
            "message": o.message,
            "issue": o.issue,
        }),
        _ => return None,
    };
    Some(
        blake3::hash(semantic.to_string().as_bytes())
            .to_hex()
            .to_string(),
    )
}

/// `BTreeSet` is an internal helper exposed for callers; not currently used by
/// op-shape definitions but kept here so the module is the single import point.
#[allow(dead_code)]
pub(crate) type _StringSet = BTreeSet<String>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_round_trip() {
        let op = make_create(
            "alice".into(),
            "bd-01TEST".into(),
            ScalarSet {
                title: Some("Hi".into()),
                status: Some(Status::Open),
                priority: Some(1),
                ..Default::default()
            },
            "2026-04-20T18:24:55.124583Z".parse().unwrap(),
        );
        let v = serde_json::to_value(&op).unwrap();
        // top-level shape
        assert_eq!(v["kind"], "create");
        assert_eq!(v["v"], 1);
        assert_eq!(v["entity"], "bd-01TEST");
        assert_eq!(v["set"]["title"], "Hi");
        assert_eq!(v["set"]["status"], "open");
        assert_eq!(v["set"]["priority"], 1);

        let back: Op = serde_json::from_value(v).unwrap();
        match back {
            Op::Create(c) => {
                assert_eq!(c.entity, "bd-01TEST");
                assert_eq!(c.set.title, Some("Hi".to_string()));
                assert_eq!(c.set.status, Some(Status::Open));
            }
            _ => panic!("expected Create"),
        }
    }

    #[test]
    fn patch_with_expect() {
        let v = json!({
            "v": 1, "op": "x", "ts": "2026-04-20T18:24:55.124583Z",
            "actor": "a", "kind": "patch",
            "entity": "bd-1",
            "expect": {"status": "op-prev"},
            "set": {"status": "doing"},
        });
        let op: Op = serde_json::from_value(v).unwrap();
        match op {
            Op::Patch(p) => {
                assert_eq!(p.expect.get("status").map(String::as_str), Some("op-prev"));
                assert_eq!(p.set.status, Some(Status::Doing));
            }
            _ => panic!("expected Patch"),
        }
    }

    #[test]
    fn dep_add_default_kind() {
        let v = json!({
            "v": 1, "op": "x", "ts": "2026-04-20T18:24:55.124583Z",
            "actor": "a", "kind": "dep_add",
            "entity": "bd-1", "parent": "bd-2",
        });
        let op: Op = serde_json::from_value(v).unwrap();
        match op {
            Op::DepAdd(d) => assert_eq!(d.dep_kind, "blocks"),
            _ => panic!("expected DepAdd"),
        }
    }

    #[test]
    fn rel_add_default_kind() {
        let v = json!({
            "v": 1, "op": "x", "ts": "2026-04-20T18:24:55.124583Z",
            "actor": "a", "kind": "rel_add",
            "entity": "bd-1", "parent": "bd-2",
        });
        let op: Op = serde_json::from_value(v).unwrap();
        match op {
            Op::RelAdd(r) => assert_eq!(r.rel_kind, "parent"),
            _ => panic!("expected RelAdd"),
        }
    }

    #[test]
    fn rejects_unknown_kind() {
        let v = json!({
            "v": 1, "op": "x", "ts": "2026-04-20T18:24:55.124583Z",
            "actor": "a", "kind": "frobnicate",
            "entity": "bd-1",
        });
        let r: Result<Op, _> = serde_json::from_value(v);
        assert!(r.is_err());
    }

    #[test]
    fn rejects_unknown_status() {
        let v = json!({
            "v": 1, "op": "x", "ts": "2026-04-20T18:24:55.124583Z",
            "actor": "a", "kind": "create",
            "entity": "bd-1",
            "set": {"title": "t", "status": "bogus"},
        });
        let r: Result<Op, _> = serde_json::from_value(v);
        assert!(r.is_err());
    }

    #[test]
    fn note_kind_validator() {
        assert!(validate_note_kind("progress"));
        assert!(validate_note_kind("handoff"));
        assert!(!validate_note_kind("category"));
    }

    #[test]
    fn op_kind_names() {
        let add = make_tag(
            true,
            "a".into(),
            "bd-1".into(),
            "x".into(),
            "2026-04-20T18:24:55Z".parse().unwrap(),
        );
        let rem = make_tag(
            false,
            "a".into(),
            "bd-1".into(),
            "x".into(),
            "2026-04-20T18:24:55Z".parse().unwrap(),
        );
        assert_eq!(add.kind_name(), "tag_add");
        assert_eq!(rem.kind_name(), "tag_remove");
        let rel = make_rel(
            true,
            "a".into(),
            "bd-1".into(),
            "bd-2".into(),
            "parent".into(),
            "2026-04-20T18:24:55Z".parse().unwrap(),
        );
        assert_eq!(rel.kind_name(), "rel_add");
    }
}
