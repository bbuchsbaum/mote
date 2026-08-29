//! Incremental, versioned event delivery over the immutable operation log.
//!
//! Events are a read-only projection: they never become part of the durable
//! protocol. The op filename is the event id/cursor, so consumers can dedupe
//! and resume without a mutable broker or daemon.

use std::collections::BTreeSet;
use std::io::Write;
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use serde_json::Value;

use crate::errors::{MoteError, MoteResult};
use crate::op::Op;
use crate::reducer;
use crate::repo::Store;
use crate::state::State;

pub const EVENT_SCHEMA: &str = "mote.event.v1";
pub const VALID_EVENT_CATEGORIES: &[&str] = &[
    "issue",
    "claim",
    "reservation",
    "message",
    "discussion",
    "session",
    "presence",
    "candidate",
];

/// Stable JSONL envelope emitted by `mote events --json` and follow-mode
/// projections such as `mote inbox --follow --json`.
#[derive(Debug, Clone, Serialize)]
pub struct EventEnvelope {
    pub schema: &'static str,
    pub event_id: String,
    pub store_id: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub category: String,
    pub op_id: String,
    pub ts: String,
    pub actor: String,
    pub accepted: bool,
    pub data: Value,
}

#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    categories: BTreeSet<String>,
    actor: Option<String>,
}

impl EventFilter {
    pub fn new(categories: &[String], actor: Option<String>) -> MoteResult<Self> {
        let mut parsed = BTreeSet::new();
        for raw in categories {
            let category = raw.trim().to_ascii_lowercase();
            if category == "all" {
                parsed.clear();
                break;
            }
            if !VALID_EVENT_CATEGORIES.contains(&category.as_str()) {
                return Err(MoteError::Invalid(format!(
                    "invalid event kind `{raw}` (expected one of: all | {})",
                    VALID_EVENT_CATEGORIES.join(" | ")
                )));
            }
            parsed.insert(category);
        }
        Ok(Self {
            categories: parsed,
            actor: actor
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty()),
        })
    }

    pub fn messages_for(actor: &str) -> Self {
        Self {
            categories: BTreeSet::from(["message".to_string()]),
            actor: Some(actor.to_string()),
        }
    }

    fn matches(&self, op: &Op, state: &State) -> bool {
        let category = event_category(op);
        if !self.categories.is_empty() && !self.categories.contains(category) {
            return false;
        }
        self.actor
            .as_deref()
            .is_none_or(|actor| op_relates_to_actor(op, state, actor))
    }

    fn matches_projection(&self, category: &str, actor: &str) -> bool {
        (self.categories.is_empty() || self.categories.contains(category))
            && self.actor.as_deref().is_none_or(|wanted| wanted == actor)
    }
}

/// Filesystem notification plus a periodic fallback tick. `mote watch`,
/// `mote events --follow`, and `mote inbox --follow` share this primitive.
pub struct StoreWatcher {
    rx: Receiver<()>,
    _watcher: Option<RecommendedWatcher>,
}

impl StoreWatcher {
    pub fn new(store: &Store, interval_s: u64) -> MoteResult<Self> {
        Self::new_with_options(store, Duration::from_secs(interval_s.max(1)), true)
    }

    fn new_with_options(store: &Store, interval: Duration, watch_fs: bool) -> MoteResult<Self> {
        let (tx, rx) = channel::<()>();

        let watcher = if watch_fs {
            let tx_fs = tx.clone();
            let mut watcher: RecommendedWatcher =
                notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res {
                        if matches!(
                            event.kind,
                            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                        ) {
                            let _ = tx_fs.send(());
                        }
                    }
                })
                .map_err(|e| MoteError::Other(format!("watch: install watcher: {e}")))?;
            watcher
                .watch(&store.ops_dir(), RecursiveMode::NonRecursive)
                .map_err(|e| MoteError::Other(format!("watch: subscribe ops_dir: {e}")))?;
            Some(watcher)
        } else {
            None
        };

        thread::spawn(move || {
            loop {
                thread::sleep(interval);
                if tx.send(()).is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            rx,
            _watcher: watcher,
        })
    }

    pub fn wait(&self) -> bool {
        if self.rx.recv().is_err() {
            return false;
        }
        drain_for(&self.rx, Duration::from_millis(150));
        true
    }

    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        if self.rx.recv_timeout(timeout).is_err() {
            return false;
        }
        drain_for(&self.rx, Duration::from_millis(150));
        true
    }
}

/// Tracks op filenames already observed by a follow-mode consumer. It keeps a
/// set rather than only a high-water filename so a clock-regressed op that is
/// published later is still delivered during the running process.
pub struct EventTailer {
    seen: BTreeSet<String>,
    seen_derived: BTreeSet<String>,
    after_cursor: Option<String>,
    initial_names: Vec<String>,
    watcher: Option<StoreWatcher>,
    interval_s: u64,
}

impl EventTailer {
    pub fn new(store: &Store, after: Option<&str>, interval_s: u64) -> MoteResult<Self> {
        let initial_names = store.list_op_filenames()?;
        let after_cursor = after.map(cursor_filename).transpose()?;
        let seen = match after_cursor.as_deref() {
            Some(cursor) => initial_names
                .iter()
                .filter(|name| name.as_str() <= cursor)
                .cloned()
                .collect(),
            None => initial_names.iter().cloned().collect(),
        };
        Ok(Self {
            seen,
            seen_derived: BTreeSet::new(),
            after_cursor,
            initial_names,
            watcher: None,
            interval_s,
        })
    }

    pub fn initial_names(&self) -> &[String] {
        &self.initial_names
    }

    pub fn poll(&mut self, store: &Store, filter: &EventFilter) -> MoteResult<Vec<EventEnvelope>> {
        self.poll_at(store, filter, jiff::Timestamp::now())
    }

    pub fn poll_at(
        &mut self,
        store: &Store,
        filter: &EventFilter,
        now: jiff::Timestamp,
    ) -> MoteResult<Vec<EventEnvelope>> {
        let names = store.list_op_filenames()?;
        let unseen: Vec<String> = names
            .iter()
            .filter(|&name| self.seen.insert(name.clone()))
            .cloned()
            .collect();
        let mut events = accepted_events_for_names(store, &unseen, filter)?;
        let state = reducer::replay_store(store)?;
        let store_id = store.read_format()?.store_id;
        let explicit_names = if self.after_cursor.is_some() {
            names.as_slice()
        } else {
            unseen.as_slice()
        };
        let mut projected = explicit_presence_events_for_names(store, explicit_names, filter)?;
        let now_ts = crate::ids::format_rfc3339(now);
        projected.extend(derived_reservation_events(
            &state, &store_id, &now_ts, filter,
        ));
        projected.extend(derived_presence_events(&state, &store_id, &now_ts, filter));
        for event in projected {
            let key = cursor_filename(&event.event_id)?;
            if self
                .after_cursor
                .as_ref()
                .is_some_and(|cursor| key <= *cursor)
                || !self.seen_derived.insert(event.event_id.clone())
            {
                continue;
            }
            events.push(event);
        }
        events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
        Ok(events)
    }

    /// Install filesystem notifications after the caller has emitted any
    /// cursor backlog. Call `poll` once more immediately after this method to
    /// close the installation gap before blocking in `wait`.
    pub fn start(&mut self, store: &Store) -> MoteResult<()> {
        if self.watcher.is_none() {
            self.watcher = Some(StoreWatcher::new(store, self.interval_s)?);
        }
        Ok(())
    }

    pub fn wait(&self) -> bool {
        self.watcher.as_ref().is_some_and(StoreWatcher::wait)
    }

    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        self.watcher
            .as_ref()
            .is_some_and(|watcher| watcher.wait_timeout(timeout))
    }
}

pub fn accepted_events(
    store: &Store,
    after: Option<&str>,
    filter: &EventFilter,
) -> MoteResult<Vec<EventEnvelope>> {
    let names = store.list_op_filenames()?;
    let cursor = after.map(cursor_filename).transpose()?;
    let selected = match cursor.as_deref() {
        Some(cursor) => names
            .iter()
            .filter(|name| name.as_str() > cursor)
            .cloned()
            .collect::<Vec<_>>(),
        None => names.clone(),
    };
    let mut events = accepted_events_for_names(store, &selected, filter)?;
    let state = reducer::replay_store(store)?;
    let store_id = store.read_format()?.store_id;
    for event in explicit_presence_events_for_names(store, &names, filter)? {
        let key = cursor_filename(&event.event_id)?;
        if cursor.as_ref().is_none_or(|cursor| key > *cursor) {
            events.push(event);
        }
    }
    let now_ts = crate::ids::format_rfc3339(jiff::Timestamp::now());
    let mut projected = derived_reservation_events(&state, &store_id, &now_ts, filter);
    projected.extend(derived_presence_events(&state, &store_id, &now_ts, filter));
    for event in projected {
        let key = cursor_filename(&event.event_id)?;
        if cursor.as_ref().is_none_or(|cursor| key > *cursor) {
            events.push(event);
        }
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    Ok(events)
}

pub fn accepted_events_for_names(
    store: &Store,
    names: &[String],
    filter: &EventFilter,
) -> MoteResult<Vec<EventEnvelope>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let all_names = store.list_op_filenames()?;
    let state = reducer::replay_store(store)?;
    let store_id = store.read_format()?.store_id;
    let mut out = Vec::new();
    for name in names {
        let op_id = name.strip_suffix(".json").unwrap_or(name);
        if !state.was_accepted(op_id) {
            continue;
        }
        let bytes = std::fs::read(store.ops_dir().join(name))?;
        let op: Op = match serde_json::from_slice(&bytes) {
            Ok(op) => op,
            Err(_) => continue,
        };
        if filter.matches(&op, &state) {
            let invalidates_entity = matches!(
                &op,
                Op::Close(_)
                    | Op::Delete(_)
                    | Op::CandidateRevoke(_)
                    | Op::CandidateSupersede(_)
                    | Op::CandidateAbandon(_)
                    | Op::CandidateLanded(_)
            )
            .then(|| op.entity().map(str::to_string))
            .flatten();
            let reservation_transition = matches!(
                &op,
                Op::ReserveOpen(_) | Op::ReserveClose(_) | Op::ReserveAdopt(_)
            );
            let session_transition = matches!(
                &op,
                Op::SessionStart(_)
                    | Op::SessionHeartbeat(_)
                    | Op::SessionStatus(_)
                    | Op::SessionEnd(_)
            );
            let message_delivery = matches!(&op, Op::MsgSend(_));
            let discussion_notification = matches!(&op, Op::BoardPost(_));
            let mut event = event_from_op(&store_id, op)?;
            if invalidates_entity.is_some()
                || reservation_transition
                || session_transition
                || message_delivery
                || discussion_notification
            {
                let prefix: Vec<String> = all_names
                    .iter()
                    .take_while(|candidate| candidate.as_str() <= name.as_str())
                    .cloned()
                    .collect();
                let state_at_event = state_for_names(store, &prefix)?;
                if let Some(entity) = invalidates_entity {
                    add_orphaned_lease_effects(&mut event, &state_at_event, &entity);
                }
                if reservation_transition {
                    add_reservation_binding(&mut event, &state_at_event);
                }
                if session_transition {
                    add_session_snapshot(&mut event, &state_at_event);
                }
                if message_delivery {
                    add_message_delivery(&mut event, &state_at_event);
                }
                if discussion_notification {
                    add_discussion_notification(&mut event, &state_at_event);
                }
            }
            out.push(event);
        }
    }
    Ok(out)
}

fn add_message_delivery(event: &mut EventEnvelope, state: &State) {
    let Some(msg_id) = event.data["msg_id"].as_str() else {
        return;
    };
    let Some(message) = state.messages.get(msg_id) else {
        return;
    };
    if let Some(data) = event.data.as_object_mut() {
        data.insert("delivery".into(), serde_json::json!("queued"));
        data.insert("addressed".into(), serde_json::json!(true));
        data.insert("private".into(), serde_json::json!(false));
        data.insert(
            "require_live".into(),
            serde_json::json!(message.require_live),
        );
        data.insert(
            "recipient_presence".into(),
            serde_json::json!(message.recipient_presence),
        );
    }
}

fn add_discussion_notification(event: &mut EventEnvelope, state: &State) {
    let Some(post_id) = event.data["post_id"].as_str() else {
        return;
    };
    let Some(post) = state.board_posts.get(post_id) else {
        return;
    };
    if let Some(data) = event.data.as_object_mut() {
        data.insert(
            "notification_recipients".into(),
            serde_json::json!(post.notification_recipients),
        );
        data.insert("public".into(), serde_json::json!(true));
    }
}

fn add_session_snapshot(event: &mut EventEnvelope, state: &State) {
    let Some(session_id) = event.data["session_id"].as_str() else {
        return;
    };
    let Some(session) = state.sessions.get(session_id) else {
        return;
    };
    if event.event_type == "session.started" && session.started_op_id != event.op_id {
        // Legacy clients renewed with a repeated session_start. Preserve wire
        // replay compatibility while exposing the operation's actual meaning.
        event.event_type = "session.heartbeat".into();
    }
    let active_intent = if session.is_live(&event.ts) {
        session.intent.as_ref()
    } else {
        None
    };
    if let Some(data) = event.data.as_object_mut() {
        data.insert("actor".into(), serde_json::json!(session.actor));
        data.insert("started_ts".into(), serde_json::json!(session.started_ts));
        data.insert(
            "started_op_id".into(),
            serde_json::json!(session.started_op_id),
        );
        data.insert(
            "last_heartbeat_ts".into(),
            serde_json::json!(session.last_heartbeat_ts),
        );
        data.insert(
            "last_heartbeat_op_id".into(),
            serde_json::json!(session.last_heartbeat_op_id),
        );
        data.insert(
            "lease_until_ts".into(),
            serde_json::json!(session.lease_until_ts),
        );
        data.insert("ended_ts".into(), serde_json::json!(session.ended_ts));
        data.insert("ended_op_id".into(), serde_json::json!(session.ended_op_id));
        data.insert("intent".into(), serde_json::json!(active_intent));
    }
}

fn add_reservation_binding(event: &mut EventEnvelope, state: &State) {
    let Some(reservation_id) = event.data["reservation_id"].as_str() else {
        return;
    };
    let Some(reservation) = state.reservations.get(reservation_id) else {
        return;
    };
    if let Some(data) = event.data.as_object_mut() {
        let binding_kind = state.reservation_binding_kind(reservation);
        data.insert("binding_kind".into(), Value::String(binding_kind.into()));
        data.insert(
            "disposition".into(),
            serde_json::to_value(state.reservation_disposition(reservation, &event.ts))
                .expect("lease disposition serializes"),
        );
        if binding_kind == "candidate" {
            data.insert(
                "candidate_id".into(),
                Value::String(reservation.entity.clone()),
            );
        }
    }
}

fn add_orphaned_lease_effects(event: &mut EventEnvelope, state: &State, entity: &str) {
    let orphaned_claim = state.beads.get(entity).is_some_and(|bead| {
        state.claim_disposition(bead, &event.ts) == crate::state::LeaseDisposition::Orphaned
    });
    let orphaned_reservations: Vec<Value> = state
        .reservations
        .values()
        .filter(|reservation| {
            reservation.entity == entity
                && state.reservation_disposition(reservation, &event.ts)
                    == crate::state::LeaseDisposition::Orphaned
        })
        .map(|reservation| {
            serde_json::json!({
                "reservation_id": reservation.reservation_id,
                "actor": reservation.actor,
                "binding_kind": state.reservation_binding_kind(reservation),
                "paths": reservation.live_paths(),
                "lease_until_ts": reservation.lease_until_ts,
                "disposition": "orphaned",
            })
        })
        .collect();
    if let Some(data) = event.data.as_object_mut() {
        data.insert(
            "lease_effects".into(),
            serde_json::json!({
                "orphaned_claim": orphaned_claim,
                "orphaned_reservations": orphaned_reservations,
            }),
        );
    }
}

pub fn state_for_names(store: &Store, names: &[String]) -> MoteResult<State> {
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        entries.push((name.clone(), std::fs::read(store.ops_dir().join(name))?));
    }
    Ok(reducer::replay(entries))
}

fn explicit_presence_events_for_names(
    store: &Store,
    names: &[String],
    filter: &EventFilter,
) -> MoteResult<Vec<EventEnvelope>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let all_names = store.list_op_filenames()?;
    let final_state = reducer::replay_store(store)?;
    let store_id = store.read_format()?.store_id;
    let mut events = Vec::new();
    for name in names {
        let op_id = name.strip_suffix(".json").unwrap_or(name);
        if !final_state.was_accepted(op_id) {
            continue;
        }
        let op: Op = match serde_json::from_slice(&std::fs::read(store.ops_dir().join(name))?) {
            Ok(op) => op,
            Err(_) => continue,
        };
        if !matches!(
            &op,
            Op::SessionStart(_) | Op::SessionHeartbeat(_) | Op::SessionEnd(_)
        ) || !filter.matches_projection("presence", op.actor())
        {
            continue;
        }
        let before_names: Vec<String> = all_names
            .iter()
            .take_while(|candidate| candidate.as_str() < name.as_str())
            .cloned()
            .collect();
        let after_names: Vec<String> = all_names
            .iter()
            .take_while(|candidate| candidate.as_str() <= name.as_str())
            .cloned()
            .collect();
        let before = state_for_names(store, &before_names)?;
        let after = state_for_names(store, &after_names)?;
        let at: jiff::Timestamp = op
            .ts()
            .parse()
            .map_err(|error: jiff::Error| MoteError::Other(error.to_string()))?;
        let before_status = crate::actor_status::actor_status(
            &before,
            op.actor(),
            None,
            at,
            crate::actor_status::DEFAULT_RECENT_WINDOW_S,
        );
        let after_status = crate::actor_status::actor_status(
            &after,
            op.actor(),
            None,
            at,
            crate::actor_status::DEFAULT_RECENT_WINDOW_S,
        );
        let transition =
            if before_status.presence.state != "live" && after_status.presence.state == "live" {
                Some(("presence.live", "live", "session_lease", "lease_valid"))
            } else if matches!(&op, Op::SessionEnd(_))
                && before_status.presence.state == "live"
                && after_status.presence.state != "live"
            {
                Some(("presence.ended", "expired", "session_history", "ended"))
            } else {
                None
            };
        let Some((event_type, presence_state, source, reason)) = transition else {
            continue;
        };
        let session_id = match &op {
            Op::SessionStart(op) => op.session_id.as_str(),
            Op::SessionHeartbeat(op) => op.session_id.as_str(),
            Op::SessionEnd(op) => op.session_id.as_str(),
            _ => unreachable!(),
        };
        events.push(EventEnvelope {
            schema: EVENT_SCHEMA,
            event_id: format!(
                "{}~d-{}-{}",
                op.op_id(),
                event_type.replace('.', "-"),
                short_hash(op.actor())
            ),
            store_id: store_id.clone(),
            event_type: event_type.into(),
            category: "presence".into(),
            op_id: op.op_id().to_string(),
            ts: op.ts().to_string(),
            actor: op.actor().to_string(),
            accepted: true,
            data: serde_json::json!({
                "derived": true,
                "actor": op.actor(),
                "session_id": session_id,
                "presence_state": presence_state,
                "source": source,
                "reason": reason,
                "as_of_ts": op.ts(),
                "live_session_count": after_status.presence.live_session_count,
                "latest_lease_until_ts": after_status.presence.latest_lease_until_ts,
                "trigger_op_id": op.op_id(),
            }),
        });
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    Ok(events)
}

fn derived_presence_events(
    state: &State,
    store_id: &str,
    now_ts: &str,
    filter: &EventFilter,
) -> Vec<EventEnvelope> {
    let Ok(now) = now_ts.parse::<jiff::Timestamp>() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    for actor in crate::actor_status::known_actor_names(state) {
        if !filter.matches_projection("presence", &actor)
            || !state
                .sessions
                .values()
                .any(|session| session.actor == actor)
        {
            continue;
        }
        let status = crate::actor_status::actor_status(
            state,
            &actor,
            None,
            now,
            crate::actor_status::DEFAULT_RECENT_WINDOW_S,
        );
        if status.presence.state == "live" {
            let Some(session) = status
                .sessions
                .iter()
                .filter(|session| session.live)
                .max_by(|a, b| {
                    a.lease_until_ts
                        .cmp(&b.lease_until_ts)
                        .then_with(|| a.session_id.cmp(&b.session_id))
                })
            else {
                continue;
            };
            let Ok(deadline) = session.lease_until_ts.parse::<jiff::Timestamp>() else {
                continue;
            };
            let warning_seconds = if session.ttl_s > 1 {
                (session.ttl_s / 10).max(1)
            } else {
                0
            };
            let Ok(warning_at) =
                deadline.checked_sub(jiff::SignedDuration::from_secs(warning_seconds.into()))
            else {
                continue;
            };
            if now < warning_at {
                continue;
            }
            let scheduled_ts = crate::ids::format_rfc3339(warning_at);
            events.push(presence_boundary_event(
                store_id,
                "presence.expiring",
                "live",
                "session_lease",
                "ttl_near_deadline",
                &actor,
                &session.last_heartbeat_op_id,
                &session.session_id,
                &scheduled_ts,
                now_ts,
                &session.lease_until_ts,
            ));
        } else if status.presence.state == "expired" {
            let terminal = status
                .sessions
                .iter()
                .map(|session| {
                    let deadline = session.lease_until_ts.as_str();
                    match session.ended_ts.as_deref() {
                        Some(ended) if ended <= deadline => (ended, "ended", session),
                        _ => (deadline, "expired", session),
                    }
                })
                .max_by(|a, b| {
                    a.0.cmp(b.0)
                        .then_with(|| a.2.session_id.cmp(&b.2.session_id))
                });
            let Some((scheduled_ts, terminal_kind, session)) = terminal else {
                continue;
            };
            if terminal_kind != "expired" || scheduled_ts > now_ts {
                continue;
            }
            events.push(presence_boundary_event(
                store_id,
                "presence.expired",
                "expired",
                "session_history",
                "ttl_elapsed",
                &actor,
                &session.last_heartbeat_op_id,
                &session.session_id,
                scheduled_ts,
                now_ts,
                &session.lease_until_ts,
            ));
        }
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    events
}

#[allow(clippy::too_many_arguments)]
fn presence_boundary_event(
    store_id: &str,
    event_type: &str,
    presence_state: &str,
    source: &str,
    reason: &str,
    actor: &str,
    op_id: &str,
    session_id: &str,
    scheduled_ts: &str,
    as_of_ts: &str,
    deadline: &str,
) -> EventEnvelope {
    let compact_ts = scheduled_ts.replace(['-', ':'], "");
    EventEnvelope {
        schema: EVENT_SCHEMA,
        event_id: format!(
            "{compact_ts}-d-{}-{}-{}",
            event_type.replace('.', "-"),
            short_hash(actor),
            short_hash(op_id)
        ),
        store_id: store_id.to_string(),
        event_type: event_type.into(),
        category: "presence".into(),
        op_id: op_id.to_string(),
        ts: scheduled_ts.to_string(),
        actor: actor.to_string(),
        accepted: true,
        data: serde_json::json!({
            "derived": true,
            "actor": actor,
            "session_id": session_id,
            "presence_state": presence_state,
            "source": source,
            "reason": reason,
            "as_of_ts": as_of_ts,
            "deadline": deadline,
            "trigger_op_id": op_id,
        }),
    }
}

fn short_hash(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..12].to_string()
}

fn derived_reservation_events(
    state: &State,
    store_id: &str,
    now_ts: &str,
    filter: &EventFilter,
) -> Vec<EventEnvelope> {
    use crate::state::ReservationExpiryPhase;

    let mut events = Vec::new();
    for reservation in state.reservations.values() {
        let Some(phase) = state.reservation_expiry_phase(reservation, now_ts) else {
            continue;
        };
        let (event_type, scheduled_ts, reason) = match phase {
            ReservationExpiryPhase::Expiring => (
                "reservation.expiring",
                state
                    .reservation_warning_ts(reservation)
                    .unwrap_or_else(|| reservation.lease_until_ts.clone()),
                "ttl_near_deadline",
            ),
            ReservationExpiryPhase::Expired => (
                "reservation.expired",
                reservation.lease_until_ts.clone(),
                "ttl_elapsed",
            ),
        };
        let compact_ts = scheduled_ts.replace(['-', ':'], "");
        let event_id = format!(
            "{compact_ts}-d-reservation-{}-{}",
            phase.as_str(),
            reservation.reservation_id
        );
        let binding_kind = state.reservation_binding_kind(reservation);
        let event = EventEnvelope {
            schema: EVENT_SCHEMA,
            event_id,
            store_id: store_id.to_string(),
            event_type: event_type.into(),
            category: "reservation".into(),
            op_id: reservation.opened_op_id.clone(),
            ts: scheduled_ts,
            actor: reservation.actor.clone(),
            accepted: true,
            data: serde_json::json!({
                "derived": true,
                "reservation_id": reservation.reservation_id,
                "holder": reservation.actor,
                "entity": reservation.entity,
                "bead": (binding_kind == "bead").then_some(&reservation.entity),
                "candidate_id": (binding_kind == "candidate").then_some(&reservation.entity),
                "binding_kind": binding_kind,
                "paths": reservation.live_paths(),
                "deadline": reservation.lease_until_ts,
                "reason": reason,
            }),
        };
        let category_matches =
            filter.categories.is_empty() || filter.categories.contains(event.category.as_str());
        let actor_matches = filter
            .actor
            .as_deref()
            .is_none_or(|actor| actor == event.actor);
        if category_matches && actor_matches {
            events.push(event);
        }
    }
    events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
    events
}

pub fn write_event(event: &EventEnvelope, json_mode: bool) -> MoteResult<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    if json_mode {
        writeln!(out, "{}", serde_json::to_string(event)?)?;
    } else {
        writeln!(
            out,
            "{}  {}  type={}  actor={}  {}",
            event.event_id, event.ts, event.event_type, event.actor, event.data
        )?;
    }
    out.flush()?;
    Ok(())
}

/// Write an inbox delivery as stable JSON or a compact, single-line human
/// notification. The human projection stays deliberately separate from the
/// generic event view so scripts can continue to rely on `mote.event.v1`.
pub fn write_inbox_event(event: &EventEnvelope, json_mode: bool) -> MoteResult<()> {
    if json_mode {
        return write_event(event, true);
    }

    let kind = event.data["msg_kind"]
        .as_str()
        .unwrap_or(event.event_type.as_str());
    let to = event.data["to"].as_str().unwrap_or("?");
    let msg_id = event.data["msg_id"].as_str().unwrap_or("?");
    let body = event.data["body"]
        .as_str()
        .unwrap_or("")
        .replace(['\n', '\r'], " ");
    let issue = event.data["entity"]
        .as_str()
        .map(|value| format!("  issue={value}"))
        .unwrap_or_default();
    let reply = event.data["reply_to"]
        .as_str()
        .map(|value| format!("  reply-to={value}"))
        .unwrap_or_default();

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(
        out,
        "[{kind}] {} -> {to}  msg={msg_id}{issue}{reply}  {body}",
        event.actor
    )?;
    out.flush()?;
    Ok(())
}

fn event_from_op(store_id: &str, op: Op) -> MoteResult<EventEnvelope> {
    let event_id = op.op_id().to_string();
    let event_type = event_type(&op).to_string();
    let category = event_category(&op).to_string();
    let op_id = op.op_id().to_string();
    let ts = op.ts().to_string();
    let actor = op.actor().to_string();
    let request_state = match &op {
        Op::MsgSend(o) if o.reply_to.is_none() && o.msg_kind == "request" => Some("open"),
        Op::MsgSend(o) if o.msg_kind == "response" || !o.answers.is_empty() => Some("responded"),
        Op::MsgSend(o) if o.msg_kind == "decline" => Some("declined"),
        Op::MsgResolve(_) => Some("resolved"),
        _ => None,
    };
    let mut data = serde_json::to_value(&op)?;
    if let Some(obj) = data.as_object_mut() {
        for key in ["v", "op", "ts", "actor", "kind"] {
            obj.remove(key);
        }
        if let Some(request_state) = request_state {
            obj.insert(
                "request_state".to_string(),
                Value::String(request_state.to_string()),
            );
        }
    }
    Ok(EventEnvelope {
        schema: EVENT_SCHEMA,
        event_id,
        store_id: store_id.to_string(),
        event_type,
        category,
        op_id,
        ts,
        actor,
        accepted: true,
        data,
    })
}

fn event_category(op: &Op) -> &'static str {
    match op {
        Op::Claim(_) | Op::Release(_) => "claim",
        Op::MsgSend(_) | Op::MsgAck(_) | Op::MsgResolve(_) => "message",
        Op::BoardPost(_)
        | Op::BoardRead(_)
        | Op::BoardWatch(_)
        | Op::BoardTopic(_)
        | Op::BoardSticky(_)
        | Op::BoardSupersede(_)
        | Op::BoardRetract(_)
        | Op::BoardRoute(_) => "discussion",
        Op::SessionStart(_)
        | Op::SessionHeartbeat(_)
        | Op::SessionStatus(_)
        | Op::SessionEnd(_) => "session",
        Op::ReserveOpen(_) | Op::ReserveClose(_) | Op::ReserveAdopt(_) => "reservation",
        Op::CandidatePropose(_)
        | Op::CandidateEvidence(_)
        | Op::CandidateReview(_)
        | Op::CandidateAuthorize(_)
        | Op::CandidateRevoke(_)
        | Op::CandidateSupersede(_)
        | Op::CandidateAbandon(_)
        | Op::CandidateLanded(_) => "candidate",
        _ => "issue",
    }
}

fn event_type(op: &Op) -> &'static str {
    match op {
        Op::Create(_) => "issue.created",
        Op::Patch(_) => "issue.patched",
        Op::TagAdd(_) => "issue.tag_added",
        Op::TagRemove(_) => "issue.tag_removed",
        Op::DepAdd(_) => "issue.dependency_added",
        Op::DepRemove(_) => "issue.dependency_removed",
        Op::RelAdd(_) => "issue.relationship_added",
        Op::RelRemove(_) => "issue.relationship_removed",
        Op::Note(_) => "issue.noted",
        Op::Close(_) => "issue.closed",
        Op::Delete(_) => "issue.deleted",
        Op::Claim(_) => "claim.acquired",
        Op::Release(_) => "claim.released",
        Op::MsgSend(o) if o.msg_kind == "response" || !o.answers.is_empty() => "message.responded",
        Op::MsgSend(o) if o.msg_kind == "decline" => "message.declined",
        Op::MsgSend(_) => "message.sent",
        Op::MsgAck(_) => "message.acknowledged",
        Op::MsgResolve(_) => "message.resolved",
        Op::BoardPost(o) if o.post_kind.as_deref() == Some("decision") => "discussion.decided",
        Op::BoardPost(o) if o.post_kind.as_deref() == Some("summary") => "discussion.summarized",
        Op::BoardPost(_) => "discussion.posted",
        Op::BoardRead(_) => "discussion.read",
        Op::BoardWatch(o) if o.watching => "discussion.watched",
        Op::BoardWatch(_) => "discussion.unwatched",
        Op::BoardTopic(_) => "discussion.topic_created",
        Op::BoardSticky(o) if o.sticky => "discussion.post_stuck",
        Op::BoardSticky(_) => "discussion.post_unstuck",
        Op::BoardSupersede(_) => "discussion.post_superseded",
        Op::BoardRetract(_) => "discussion.post_retracted",
        Op::BoardRoute(o) if o.route_state == "routed" => "discussion.routed",
        Op::BoardRoute(o) if o.route_state == "resolved" => "discussion.resolved",
        Op::BoardRoute(o) if o.route_state == "needs_bead" => "discussion.needs_bead",
        Op::BoardRoute(_) => "discussion.route_cleared",
        Op::SessionStart(_) => "session.started",
        Op::SessionHeartbeat(_) => "session.heartbeat",
        Op::SessionStatus(_) => "session.status_changed",
        Op::SessionEnd(_) => "session.ended",
        Op::ReserveOpen(_) => "reservation.opened",
        Op::ReserveClose(_) => "reservation.closed",
        Op::ReserveAdopt(_) => "reservation.adopted",
        Op::CandidatePropose(_) => "candidate.proposed",
        Op::CandidateEvidence(_) => "candidate.evidence_recorded",
        Op::CandidateReview(_) => "candidate.reviewed",
        Op::CandidateAuthorize(_) => "candidate.authorized",
        Op::CandidateRevoke(_) => "candidate.authorization_revoked",
        Op::CandidateSupersede(_) => "candidate.superseded",
        Op::CandidateAbandon(_) => "candidate.abandoned",
        Op::CandidateLanded(_) => "candidate.landed",
    }
}

fn op_relates_to_actor(op: &Op, state: &State, actor: &str) -> bool {
    if op.actor() == actor {
        return true;
    }
    match op {
        Op::Claim(o) => o.to == actor,
        Op::MsgSend(o) => o.to == actor,
        Op::MsgAck(o) => state
            .messages
            .get(&o.msg_id)
            .is_some_and(|m| m.from == actor || m.to == actor),
        Op::MsgResolve(o) => state
            .messages
            .get(&o.msg_id)
            .is_some_and(|m| m.from == actor || m.to == actor),
        Op::BoardPost(o) => state.board_posts.get(&o.post_id).is_some_and(|post| {
            post.notification_recipients
                .iter()
                .any(|recipient| recipient == actor)
        }),
        Op::CandidatePropose(o) => {
            o.authorizer == actor || o.reviewers.iter().any(|reviewer| reviewer == actor)
        }
        Op::CandidateEvidence(_)
        | Op::CandidateReview(_)
        | Op::CandidateAuthorize(_)
        | Op::CandidateRevoke(_)
        | Op::CandidateSupersede(_)
        | Op::CandidateAbandon(_)
        | Op::CandidateLanded(_) => op
            .entity()
            .and_then(|candidate_id| state.candidates.get(candidate_id))
            .is_some_and(|candidate| {
                candidate.proposer == actor
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
                        })
            }),
        _ => false,
    }
}

fn cursor_filename(cursor: &str) -> MoteResult<String> {
    let cursor = cursor.trim();
    if cursor.is_empty()
        || cursor.contains('/')
        || cursor.contains('\\')
        || cursor.contains('\n')
        || cursor.contains('\r')
    {
        return Err(MoteError::Invalid("event cursor must be an op id".into()));
    }
    Ok(if cursor.ends_with(".json") {
        cursor.to_string()
    } else {
        format!("{cursor}.json")
    })
}

fn drain_for(rx: &Receiver<()>, window: Duration) {
    let deadline = Instant::now() + window;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if rx.recv_timeout(remaining).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use tempfile::TempDir;

    use super::*;
    use crate::ids;
    use crate::op::{ScalarSet, make_close, make_create, make_msg_send, make_reserve_open};
    use crate::publish;

    #[test]
    fn fallback_tick_wakes_a_tailer_without_fs_notifications() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        let initial = store.list_op_filenames().unwrap();
        let watcher =
            StoreWatcher::new_with_options(&store, Duration::from_millis(20), false).unwrap();

        let msg_id = ids::new_msg_id();
        let op = make_msg_send(
            "alice".into(),
            msg_id,
            "bob".into(),
            None,
            None,
            "request".into(),
            "take tests".into(),
            Timestamp::now(),
        );
        publish::publish_op(&store, &op).unwrap();

        assert!(watcher.wait());
        let names = store.list_op_filenames().unwrap();
        assert_eq!(names.len(), initial.len() + 1);
    }

    #[test]
    fn message_event_contains_full_payload_and_stable_schema() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        let msg_id = ids::new_msg_id();
        let op = make_msg_send(
            "alice".into(),
            msg_id.clone(),
            "bob".into(),
            None,
            None,
            "request".into(),
            "take tests".into(),
            Timestamp::now(),
        );
        publish::publish_op(&store, &op).unwrap();

        let events = accepted_events(&store, None, &EventFilter::messages_for("bob")).unwrap();
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event.schema, EVENT_SCHEMA);
        assert_eq!(event.event_type, "message.sent");
        assert_eq!(event.data["msg_id"], msg_id);
        assert_eq!(event.data["to"], "bob");
        assert_eq!(event.data["body"], "take tests");
    }

    #[test]
    fn derived_reservation_expiry_is_cursorable_exactly_once_and_close_suppresses_it() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        let bead = ids::new_bead_id();
        let reservation_id = ids::new_reservation_id();
        let created_at: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        let opened_at: Timestamp = "2026-01-01T00:00:01Z".parse().unwrap();
        let warning_at: Timestamp = "2026-01-01T00:00:10Z".parse().unwrap();
        let deadline: Timestamp = "2026-01-01T00:00:11Z".parse().unwrap();
        publish::publish_op(
            &store,
            &make_create(
                "alice".into(),
                bead.clone(),
                ScalarSet {
                    title: Some("expiry".into()),
                    ..Default::default()
                },
                created_at,
            ),
        )
        .unwrap();
        publish::publish_op(
            &store,
            &make_reserve_open(
                "alice".into(),
                reservation_id.clone(),
                bead.clone(),
                vec!["src/expiry.rs".into()],
                10,
                opened_at,
            ),
        )
        .unwrap();
        let filter = EventFilter::new(&["reservation".into()], None).unwrap();
        let mut tailer = EventTailer::new(&store, None, 1).unwrap();

        let warning = tailer.poll_at(&store, &filter, warning_at).unwrap();
        assert_eq!(warning.len(), 1);
        assert_eq!(warning[0].event_type, "reservation.expiring");
        assert_eq!(warning[0].data["reservation_id"], reservation_id);
        assert_eq!(warning[0].data["holder"], "alice");
        assert_eq!(warning[0].data["bead"], bead);
        assert_eq!(warning[0].data["paths"][0], "src/expiry.rs");
        assert_eq!(warning[0].data["deadline"], "2026-01-01T00:00:11.000000Z");
        assert!(
            tailer
                .poll_at(&store, &filter, warning_at)
                .unwrap()
                .is_empty()
        );

        let warning_cursor = warning[0].event_id.clone();
        let mut resumed = EventTailer::new(&store, Some(&warning_cursor), 1).unwrap();
        assert!(
            resumed
                .poll_at(&store, &filter, warning_at)
                .unwrap()
                .is_empty()
        );
        let expired = resumed.poll_at(&store, &filter, deadline).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].event_type, "reservation.expired");
        assert_eq!(expired[0].data["reason"], "ttl_elapsed");
        assert!(
            resumed
                .poll_at(&store, &filter, deadline)
                .unwrap()
                .is_empty()
        );
        let expired_cursor = expired[0].event_id.clone();
        let mut after_expiry = EventTailer::new(&store, Some(&expired_cursor), 1).unwrap();
        assert!(
            after_expiry
                .poll_at(&store, &filter, deadline)
                .unwrap()
                .is_empty()
        );

        let close_at: Timestamp = "2026-01-01T00:00:10.5Z".parse().unwrap();
        publish::publish_op(
            &store,
            &make_close("alice".into(), bead, Default::default(), close_at),
        )
        .unwrap();
        // Closing the bead orphans a reservation but does not close it; only
        // an explicit reservation close suppresses TTL events. Verify that
        // distinction by closing the reservation itself below.
        let close_reservation =
            crate::op::make_reserve_close("alice".into(), reservation_id, None, close_at);
        publish::publish_op(&store, &close_reservation).unwrap();
        let state = reducer::replay_store(&store).unwrap();
        assert!(
            derived_reservation_events(
                &state,
                &store.read_format().unwrap().store_id,
                "2026-01-01T00:00:11Z",
                &filter,
            )
            .is_empty()
        );
    }
}
