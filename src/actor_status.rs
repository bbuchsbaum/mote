//! Deterministic, replay-derived actor presence and coordination status.

use std::collections::BTreeSet;

use jiff::Timestamp;
use serde::Serialize;

use crate::candidate::CandidatePhase;
use crate::ids;
use crate::state::{HistoryEntry, LeaseDisposition, RequestState, SessionRecord, State};

pub const ACTOR_STATUS_SCHEMA: &str = "mote.actor-status.v1";
pub const DEFAULT_RECENT_WINDOW_S: u32 = 600;

#[derive(Debug, Clone, Serialize)]
pub struct ActorStatus {
    pub schema: &'static str,
    pub actor: String,
    pub known: bool,
    pub current: bool,
    pub as_of_ts: String,
    pub recent_window_s: u32,
    pub presence: PresenceStatus,
    pub activity: ActivityStatus,
    pub sessions: Vec<SessionStatus>,
    pub intent: IntentAggregate,
    pub work: WorkStatus,
    pub attention: AttentionStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct PresenceStatus {
    pub state: String,
    pub source: String,
    pub reason: String,
    pub live_session_count: usize,
    pub latest_lease_until_ts: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityStatus {
    pub recent: bool,
    pub last_observed: Option<ActivityEvidence>,
    pub last_work: Option<ActivityEvidence>,
    pub last_interaction: Option<ActivityEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityEvidence {
    pub ts: String,
    pub op_id: String,
    pub category: String,
    #[serde(rename = "type")]
    pub event_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStatus {
    pub session_id: String,
    pub actor: String,
    pub label: Option<String>,
    pub pid: Option<u32>,
    pub ttl_s: u32,
    pub started_ts: String,
    pub started_op_id: String,
    pub last_heartbeat_ts: String,
    pub last_heartbeat_op_id: String,
    pub lease_until_ts: String,
    pub ended_ts: Option<String>,
    pub ended_op_id: Option<String>,
    pub live: bool,
    pub intent: Option<crate::state::SessionIntentRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentAggregate {
    pub states: Vec<String>,
    pub mixed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkStatus {
    pub active_claims: Vec<String>,
    pub orphaned_claims: Vec<String>,
    pub active_reservations: Vec<String>,
    pub orphaned_reservations: Vec<String>,
    pub doing_beads: Vec<String>,
    pub candidates: Vec<CandidateWork>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateWork {
    pub candidate_id: String,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttentionStatus {
    pub inbox_unacked: usize,
    pub incoming_open_requests: usize,
    pub discussion_unread: usize,
    pub topic_notifications_unread: usize,
    pub watched_topics: Vec<String>,
}

/// Every actor named by accepted derived state. Querying a name does not add
/// it to this set; callers separately mark the currently resolved actor known.
pub fn known_actor_names(state: &State) -> BTreeSet<String> {
    let mut actors = BTreeSet::new();
    for entry in state
        .history
        .values()
        .flatten()
        .chain(state.orphan_history.iter())
        .filter(|entry| entry.accepted && entry.actor != "?")
    {
        actors.insert(entry.actor.clone());
    }
    for message in state.messages.values() {
        actors.insert(message.from.clone());
        actors.insert(message.to.clone());
    }
    for bead in state.beads.values() {
        if let Some(assignee) = &bead.assignee {
            actors.insert(assignee.clone());
        }
        if let Some(claim) = &bead.claim {
            actors.insert(claim.claimed_by.clone());
        }
    }
    for reservation in state.reservations.values() {
        actors.insert(reservation.actor.clone());
    }
    for session in state.sessions.values() {
        actors.insert(session.actor.clone());
    }
    for watch in state.board_topic_watches.values() {
        actors.insert(watch.actor.clone());
    }
    for post in state.board_posts.values() {
        actors.extend(post.explicit_notify.iter().cloned());
        actors.extend(post.notification_recipients.iter().cloned());
    }
    for candidate in state.candidates.values() {
        actors.insert(candidate.proposer.clone());
        actors.insert(candidate.authorizer.clone());
        actors.extend(candidate.reviewers.iter().cloned());
        if let Some(authorization) = &candidate.authorization {
            actors.extend(authorization.grantees.iter().cloned());
        }
    }
    actors
}

pub fn actor_status(
    state: &State,
    actor: &str,
    current: Option<&str>,
    as_of: Timestamp,
    recent_window_s: u32,
) -> ActorStatus {
    let as_of_ts = ids::format_rfc3339(as_of);
    let cutoff = as_of
        .checked_sub(jiff::SignedDuration::from_secs(recent_window_s.into()))
        .map(ids::format_rfc3339)
        .unwrap_or_default();
    let sessions = session_statuses(state, actor, &as_of_ts);
    let live_sessions: Vec<&SessionStatus> = sessions.iter().filter(|s| s.live).collect();
    let latest_lease_until_ts = live_sessions
        .iter()
        .map(|session| session.lease_until_ts.as_str())
        .max()
        .map(str::to_string);

    let (last_observed, last_work, last_interaction) = activity(state, actor, &as_of_ts);
    let recent = last_work
        .as_ref()
        .into_iter()
        .chain(last_interaction.as_ref())
        .any(|evidence| evidence.ts.as_str() >= cutoff.as_str());
    let presence = if !live_sessions.is_empty() {
        PresenceStatus {
            state: "live".into(),
            source: "session_lease".into(),
            reason: "lease_valid".into(),
            live_session_count: live_sessions.len(),
            latest_lease_until_ts,
        }
    } else if !sessions.is_empty() {
        let ended = sessions
            .iter()
            .filter(|session| session.ended_ts.is_some())
            .count();
        PresenceStatus {
            state: "expired".into(),
            source: "session_history".into(),
            reason: if ended == sessions.len() {
                "ended"
            } else if ended == 0 {
                "ttl_elapsed"
            } else {
                "mixed"
            }
            .into(),
            live_session_count: 0,
            latest_lease_until_ts: None,
        }
    } else if recent {
        PresenceStatus {
            state: "recent".into(),
            source: "accepted_activity".into(),
            reason: "sessionless_recent_activity".into(),
            live_session_count: 0,
            latest_lease_until_ts: None,
        }
    } else {
        PresenceStatus {
            state: "untracked".into(),
            source: "none".into(),
            reason: "no_presence_evidence".into(),
            live_session_count: 0,
            latest_lease_until_ts: None,
        }
    };

    let intent_states: BTreeSet<String> = live_sessions
        .iter()
        .filter_map(|session| session.intent.as_ref().map(|intent| intent.state.clone()))
        .collect();
    let intent = IntentAggregate {
        mixed: intent_states.len() > 1,
        states: intent_states.into_iter().collect(),
    };

    ActorStatus {
        schema: ACTOR_STATUS_SCHEMA,
        actor: actor.to_string(),
        known: actor_known_at(state, actor, current, &as_of_ts),
        current: current == Some(actor),
        as_of_ts: as_of_ts.clone(),
        recent_window_s,
        presence,
        activity: ActivityStatus {
            recent,
            last_observed,
            last_work,
            last_interaction,
        },
        sessions,
        intent,
        work: work_status(state, actor, &as_of_ts),
        attention: attention_status(state, actor),
    }
}

pub fn actor_statuses(
    state: &State,
    current: Option<&str>,
    as_of: Timestamp,
    recent_window_s: u32,
) -> Vec<ActorStatus> {
    let mut actors = known_actor_names(state);
    if let Some(current) = current {
        actors.insert(current.to_string());
    }
    actors
        .into_iter()
        .map(|actor| actor_status(state, &actor, current, as_of, recent_window_s))
        .collect()
}

fn actor_known_at(state: &State, actor: &str, current: Option<&str>, as_of_ts: &str) -> bool {
    if current == Some(actor) {
        return true;
    }
    if state
        .history
        .values()
        .flatten()
        .chain(state.orphan_history.iter())
        .any(|entry| entry.accepted && entry.actor == actor && entry.ts.as_str() <= as_of_ts)
    {
        return true;
    }
    if state.messages.values().any(|message| {
        message.sent_ts.as_str() <= as_of_ts && (message.from == actor || message.to == actor)
    }) {
        return true;
    }
    if state.board_posts.values().any(|post| {
        post.sent_ts.as_str() <= as_of_ts
            && (post
                .explicit_notify
                .iter()
                .any(|recipient| recipient == actor)
                || post
                    .notification_recipients
                    .iter()
                    .any(|recipient| recipient == actor))
    }) {
        return true;
    }
    if state
        .sessions
        .values()
        .any(|session| session.actor == actor && session.started_ts.as_str() <= as_of_ts)
        || state.reservations.values().any(|reservation| {
            reservation.actor == actor && reservation.opened_ts.as_str() <= as_of_ts
        })
    {
        return true;
    }
    if state.beads.values().any(|bead| {
        bead.created_at_ts.as_str() <= as_of_ts
            && (bead.assignee.as_deref() == Some(actor)
                || bead.claim.as_ref().is_some_and(|claim| {
                    claim.claimed_by == actor && op_visible_at(state, &claim.claim_clock, as_of_ts)
                }))
    }) {
        return true;
    }
    state.candidates.values().any(|candidate| {
        op_visible_at(state, &candidate.proposal_op_id, as_of_ts)
            && (candidate.proposer == actor
                || candidate.authorizer == actor
                || candidate.reviewers.iter().any(|reviewer| reviewer == actor)
                || candidate
                    .authorization
                    .as_ref()
                    .is_some_and(|authorization| {
                        authorization
                            .grantees
                            .iter()
                            .any(|grantee| grantee == actor)
                    }))
    })
}

fn op_visible_at(state: &State, op_id: &str, as_of_ts: &str) -> bool {
    state
        .history
        .values()
        .flatten()
        .chain(state.orphan_history.iter())
        .any(|entry| entry.accepted && entry.op_id == op_id && entry.ts.as_str() <= as_of_ts)
}

fn session_statuses(state: &State, actor: &str, as_of_ts: &str) -> Vec<SessionStatus> {
    let mut sessions: Vec<SessionStatus> = state
        .sessions
        .values()
        .filter(|session| session.actor == actor && session.started_ts.as_str() <= as_of_ts)
        .map(|session| session_status_at(session, as_of_ts))
        .collect();
    sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    sessions
}

fn session_status_at(session: &SessionRecord, as_of_ts: &str) -> SessionStatus {
    let heartbeat = session
        .heartbeats
        .iter()
        .filter(|heartbeat| heartbeat.ts.as_str() <= as_of_ts)
        .max_by(|a, b| a.op_id.cmp(&b.op_id));
    let (ttl_s, label, pid, last_heartbeat_ts, last_heartbeat_op_id, lease_until_ts) =
        match heartbeat {
            Some(heartbeat) => (
                heartbeat.ttl_s,
                heartbeat.label.clone(),
                heartbeat.pid,
                heartbeat.ts.clone(),
                heartbeat.op_id.clone(),
                heartbeat.lease_until_ts.clone(),
            ),
            None => (
                session.started_ttl_s,
                session.started_label.clone(),
                session.started_pid,
                session.started_ts.clone(),
                session.started_op_id.clone(),
                session.started_lease_until_ts.clone(),
            ),
        };
    let ended = session
        .ended_ts
        .as_deref()
        .filter(|ended_ts| *ended_ts <= as_of_ts);
    let live = ended.is_none() && as_of_ts < lease_until_ts.as_str();
    let intent = if live {
        session
            .intents
            .iter()
            .filter(|intent| intent.set_ts.as_str() <= as_of_ts)
            .max_by(|a, b| a.set_op_id.cmp(&b.set_op_id))
            .cloned()
    } else {
        None
    };
    SessionStatus {
        session_id: session.session_id.clone(),
        actor: session.actor.clone(),
        label,
        pid,
        ttl_s,
        started_ts: session.started_ts.clone(),
        started_op_id: session.started_op_id.clone(),
        last_heartbeat_ts,
        last_heartbeat_op_id,
        lease_until_ts,
        ended_ts: ended.map(str::to_string),
        ended_op_id: ended.and(session.ended_op_id.clone()),
        live,
        intent,
    }
}

fn activity(
    state: &State,
    actor: &str,
    as_of_ts: &str,
) -> (
    Option<ActivityEvidence>,
    Option<ActivityEvidence>,
    Option<ActivityEvidence>,
) {
    let entries = state
        .history
        .values()
        .flatten()
        .chain(state.orphan_history.iter())
        .filter(|entry| entry.accepted && entry.actor == actor && entry.ts.as_str() <= as_of_ts);
    let mut observed = None;
    let mut work = None;
    let mut interaction = None;
    for entry in entries {
        let classes = activity_classes(&entry.kind);
        if !classes.presence && !classes.work && !classes.interaction {
            continue;
        }
        if classes.presence {
            keep_latest(&mut observed, evidence(state, entry, "presence"));
        }
        if classes.work {
            let evidence = evidence(state, entry, "work");
            keep_latest(&mut observed, evidence.clone());
            keep_latest(&mut work, evidence);
        }
        if classes.interaction {
            let evidence = evidence(state, entry, "interaction");
            keep_latest(&mut observed, evidence.clone());
            keep_latest(&mut interaction, evidence);
        }
    }
    (observed, work, interaction)
}

#[derive(Clone, Copy)]
struct ActivityClasses {
    presence: bool,
    work: bool,
    interaction: bool,
}

fn activity_classes(kind: &str) -> ActivityClasses {
    let presence = matches!(
        kind,
        "session_start" | "session_heartbeat" | "session_status" | "session_end"
    );
    let work = matches!(
        kind,
        "create"
            | "patch"
            | "tag_add"
            | "tag_remove"
            | "dep_add"
            | "dep_remove"
            | "rel_add"
            | "rel_remove"
            | "note"
            | "close"
            | "delete"
            | "claim"
            | "release"
            | "reserve_open"
            | "reserve_close"
            | "reserve_adopt"
            | "candidate_propose"
            | "candidate_evidence"
            | "candidate_review"
            | "candidate_authorize"
            | "candidate_revoke"
            | "candidate_supersede"
            | "candidate_abandon"
            | "candidate_landed"
            | "board_route"
    );
    let interaction = matches!(
        kind,
        "msg_send"
            | "msg_ack"
            | "msg_resolve"
            | "board_post"
            | "board_read"
            | "board_watch"
            | "board_topic"
            | "board_sticky"
            | "board_supersede"
            | "board_retract"
            | "board_route"
    );
    ActivityClasses {
        presence,
        work,
        interaction,
    }
}

fn evidence(state: &State, entry: &HistoryEntry, category: &str) -> ActivityEvidence {
    ActivityEvidence {
        ts: entry.ts.clone(),
        op_id: entry.op_id.clone(),
        category: category.into(),
        event_type: event_type(state, entry),
    }
}

fn keep_latest(slot: &mut Option<ActivityEvidence>, candidate: ActivityEvidence) {
    if slot
        .as_ref()
        .is_none_or(|current| candidate.op_id > current.op_id)
    {
        *slot = Some(candidate);
    }
}

fn event_type(state: &State, entry: &HistoryEntry) -> String {
    match entry.kind.as_str() {
        "create" => "issue.created",
        "patch" => "issue.patched",
        "tag_add" => "issue.tag_added",
        "tag_remove" => "issue.tag_removed",
        "dep_add" => "issue.dependency_added",
        "dep_remove" => "issue.dependency_removed",
        "rel_add" => "issue.relationship_added",
        "rel_remove" => "issue.relationship_removed",
        "note" => "issue.noted",
        "close" => "issue.closed",
        "delete" => "issue.deleted",
        "claim" => "claim.acquired",
        "release" => "claim.released",
        "msg_send" => state
            .messages
            .values()
            .find(|message| message.sent_op_id == entry.op_id)
            .map(|message| {
                if message.msg_kind == "response" || !message.answers.is_empty() {
                    "message.responded"
                } else if message.msg_kind == "decline" {
                    "message.declined"
                } else {
                    "message.sent"
                }
            })
            .unwrap_or("message.sent"),
        "msg_ack" => "message.acknowledged",
        "msg_resolve" => "message.resolved",
        "board_post" => state
            .board_posts
            .values()
            .find(|post| post.sent_op_id == entry.op_id)
            .map(|post| match post.post_kind.as_str() {
                "decision" => "discussion.decided",
                "summary" => "discussion.summarized",
                _ => "discussion.posted",
            })
            .unwrap_or("discussion.posted"),
        "board_read" => "discussion.read",
        "board_watch" => "discussion.watch_changed",
        "board_topic" => "discussion.topic_created",
        "board_sticky" => "discussion.post_stuck",
        "board_supersede" => "discussion.post_superseded",
        "board_retract" => "discussion.post_retracted",
        "board_route" => "discussion.routed",
        "session_start" => {
            if state
                .sessions
                .values()
                .any(|session| session.started_op_id == entry.op_id)
            {
                "session.started"
            } else {
                "session.heartbeat"
            }
        }
        "session_heartbeat" => "session.heartbeat",
        "session_status" => "session.status_changed",
        "session_end" => "session.ended",
        "reserve_open" => "reservation.opened",
        "reserve_close" => "reservation.closed",
        "reserve_adopt" => "reservation.adopted",
        "candidate_propose" => "candidate.proposed",
        "candidate_evidence" => "candidate.evidence_recorded",
        "candidate_review" => "candidate.reviewed",
        "candidate_authorize" => "candidate.authorized",
        "candidate_revoke" => "candidate.authorization_revoked",
        "candidate_supersede" => "candidate.superseded",
        "candidate_abandon" => "candidate.abandoned",
        "candidate_landed" => "candidate.landed",
        _ => "unknown",
    }
    .into()
}

fn work_status(state: &State, actor: &str, as_of_ts: &str) -> WorkStatus {
    let mut active_claims = Vec::new();
    let mut orphaned_claims = Vec::new();
    let mut doing_beads = Vec::new();
    for bead in state.beads.values() {
        if bead
            .claim
            .as_ref()
            .filter(|claim| claim.claimed_by == actor)
            .is_none()
        {
            continue;
        }
        match state.claim_disposition(bead, as_of_ts) {
            LeaseDisposition::Active => {
                active_claims.push(bead.id.clone());
                if bead.status == crate::op::Status::Doing && !bead.is_deleted() {
                    doing_beads.push(bead.id.clone());
                }
            }
            LeaseDisposition::Orphaned => orphaned_claims.push(bead.id.clone()),
            _ => {}
        }
    }
    let mut active_reservations = Vec::new();
    let mut orphaned_reservations = Vec::new();
    for reservation in state
        .reservations
        .values()
        .filter(|reservation| reservation.actor == actor)
    {
        match state.reservation_disposition(reservation, as_of_ts) {
            LeaseDisposition::Active => {
                active_reservations.push(reservation.reservation_id.clone())
            }
            LeaseDisposition::Orphaned => {
                orphaned_reservations.push(reservation.reservation_id.clone())
            }
            _ => {}
        }
    }
    let mut candidates = Vec::new();
    for candidate in state
        .candidates
        .values()
        .filter(|candidate| candidate.phase == CandidatePhase::Pending)
    {
        let mut roles = BTreeSet::new();
        if candidate.proposer == actor {
            roles.insert("proposer".to_string());
        }
        if candidate.authorizer == actor {
            roles.insert("authorizer".to_string());
        }
        if candidate.reviewers.iter().any(|reviewer| reviewer == actor) {
            roles.insert("reviewer".to_string());
        }
        if candidate
            .authorization
            .as_ref()
            .is_some_and(|authorization| {
                authorization
                    .grantees
                    .iter()
                    .any(|grantee| grantee == actor)
            })
        {
            roles.insert("grantee".to_string());
        }
        if !roles.is_empty() {
            candidates.push(CandidateWork {
                candidate_id: candidate.candidate_id.clone(),
                roles: roles.into_iter().collect(),
            });
        }
    }
    active_claims.sort();
    orphaned_claims.sort();
    doing_beads.sort();
    active_reservations.sort();
    orphaned_reservations.sort();
    candidates.sort_by(|a, b| a.candidate_id.cmp(&b.candidate_id));
    WorkStatus {
        active_claims,
        orphaned_claims,
        active_reservations,
        orphaned_reservations,
        doing_beads,
        candidates,
    }
}

fn attention_status(state: &State, actor: &str) -> AttentionStatus {
    AttentionStatus {
        inbox_unacked: state.inbox_for(actor).len(),
        incoming_open_requests: state
            .messages
            .values()
            .filter(|message| {
                message.to == actor && message.request_state == Some(RequestState::Open)
            })
            .count(),
        discussion_unread: state.unread_board_posts_for(actor, None).len(),
        topic_notifications_unread: state.unread_board_notifications_for(actor, None).len(),
        watched_topics: state.watched_topics_for(actor),
    }
}
