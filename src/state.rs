//! Derived state from replaying ops.
//!
//! All state lives in memory; we never persist `state.rs` data structures to
//! disk in v0.2 (snapshots are deferred). The reducer in `crate::reducer`
//! mutates this struct in filename order.

use std::collections::{BTreeMap, BTreeSet};

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
    /// Ops that did not bind to any entity (e.g. malformed JSON, or msg_send /
    /// msg_ack without an `entity` reference) live here.
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
    /// All reservations, indexed by `reservation_id`. Both live and closed.
    pub reservations: BTreeMap<String, ReservationState>,
}

#[derive(Debug, Clone)]
pub struct ReservationState {
    pub reservation_id: String,
    pub actor: String,
    pub entity: String,
    pub paths: Vec<String>,
    pub ttl_s: u32,
    pub opened_op_id: String,
    pub opened_ts: String,
    pub lease_until_ts: String,
    pub closed_paths: BTreeSet<String>,
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

#[derive(Debug, Clone)]
pub struct MsgRecord {
    pub msg_id: String,
    pub from: String,
    pub to: String,
    pub entity: Option<String>,
    pub reservation: Option<String>,
    pub msg_kind: String,
    pub body: String,
    pub sent_ts: String,
    pub sent_op_id: String,
    pub ack_op_id: Option<String>,
    pub ack_ts: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BoardPostRecord {
    pub post_id: String,
    pub from: String,
    pub topic: String,
    pub body: String,
    pub reply_to: Option<String>,
    pub sticky: bool,
    pub sticky_op_id: Option<String>,
    pub sent_ts: String,
    pub sent_op_id: String,
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

impl State {
    pub fn push_history(&mut self, entity: Option<&str>, entry: HistoryEntry) {
        match entity {
            Some(e) => self.history.entry(e.to_string()).or_default().push(entry),
            None => self.orphan_history.push(entry),
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
