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

use crate::ids;

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
    BoardPost(BoardPostOp),
    BoardRead(BoardReadOp),
    BoardTopic(BoardTopicOp),
    BoardSticky(BoardStickyOp),
    ReserveOpen(ReserveOpenOp),
    ReserveClose(ReserveCloseOp),
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
            Op::BoardPost(o) => &o.op,
            Op::BoardRead(o) => &o.op,
            Op::BoardTopic(o) => &o.op,
            Op::BoardSticky(o) => &o.op,
            Op::ReserveOpen(o) => &o.op,
            Op::ReserveClose(o) => &o.op,
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
            Op::BoardPost(o) => &o.actor,
            Op::BoardRead(o) => &o.actor,
            Op::BoardTopic(o) => &o.actor,
            Op::BoardSticky(o) => &o.actor,
            Op::ReserveOpen(o) => &o.actor,
            Op::ReserveClose(o) => &o.actor,
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
            Op::BoardPost(o) => &o.ts,
            Op::BoardRead(o) => &o.ts,
            Op::BoardTopic(o) => &o.ts,
            Op::BoardSticky(o) => &o.ts,
            Op::ReserveOpen(o) => &o.ts,
            Op::ReserveClose(o) => &o.ts,
        }
    }

    /// Returns the entity (bead id) this op acts on, or `None` for op kinds
    /// that are not entity-scoped (`msg_ack`, `reserve_close`; `msg_send` may
    /// optionally reference an entity).
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
            Op::MsgAck(_) => None,
            Op::BoardPost(_) => None,
            Op::BoardRead(_) => None,
            Op::BoardTopic(_) => None,
            Op::BoardSticky(_) => None,
            Op::ReserveOpen(o) => Some(&o.entity),
            Op::ReserveClose(_) => None,
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
            Op::BoardPost(_) => "board_post",
            Op::BoardRead(_) => "board_read",
            Op::BoardTopic(_) => "board_topic",
            Op::BoardSticky(_) => "board_sticky",
            Op::ReserveOpen(_) => "reserve_open",
            Op::ReserveClose(_) => "reserve_close",
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
        v: 1,
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

pub fn make_msg_ack(actor: String, msg_id: String, ts: jiff::Timestamp) -> Op {
    Op::MsgAck(MsgAckOp {
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
    Op::BoardPost(BoardPostOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        post_id,
        topic,
        body,
        reply_to,
    })
}

pub fn make_board_read(
    actor: String,
    upto_op_id: String,
    topic: Option<String>,
    ts: jiff::Timestamp,
) -> Op {
    Op::BoardRead(BoardReadOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(ts),
        actor,
        upto_op_id,
        topic,
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

pub const VALID_NOTE_KINDS: &[&str] = &["note", "progress", "decision", "handoff", "blocker"];
pub const VALID_MSG_KINDS: &[&str] = &["note", "request", "handoff", "blocked", "fyi"];

pub fn validate_note_kind(k: &str) -> bool {
    VALID_NOTE_KINDS.contains(&k)
}

pub fn validate_msg_kind(k: &str) -> bool {
    VALID_MSG_KINDS.contains(&k)
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
