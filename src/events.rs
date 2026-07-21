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
pub const VALID_EVENT_CATEGORIES: &[&str] =
    &["issue", "claim", "reservation", "message", "discussion"];

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
}

/// Tracks op filenames already observed by a follow-mode consumer. It keeps a
/// set rather than only a high-water filename so a clock-regressed op that is
/// published later is still delivered during the running process.
pub struct EventTailer {
    seen: BTreeSet<String>,
    initial_names: Vec<String>,
    watcher: Option<StoreWatcher>,
    interval_s: u64,
}

impl EventTailer {
    pub fn new(store: &Store, after: Option<&str>, interval_s: u64) -> MoteResult<Self> {
        let initial_names = store.list_op_filenames()?;
        let seen = match after {
            Some(cursor) => {
                let cursor = cursor_filename(cursor)?;
                initial_names
                    .iter()
                    .filter(|name| name.as_str() <= cursor.as_str())
                    .cloned()
                    .collect()
            }
            None => initial_names.iter().cloned().collect(),
        };
        Ok(Self {
            seen,
            initial_names,
            watcher: None,
            interval_s,
        })
    }

    pub fn initial_names(&self) -> &[String] {
        &self.initial_names
    }

    pub fn poll(&mut self, store: &Store, filter: &EventFilter) -> MoteResult<Vec<EventEnvelope>> {
        let names = store.list_op_filenames()?;
        let unseen: Vec<String> = names
            .into_iter()
            .filter(|name| self.seen.insert(name.clone()))
            .collect();
        accepted_events_for_names(store, &unseen, filter)
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
}

pub fn accepted_events(
    store: &Store,
    after: Option<&str>,
    filter: &EventFilter,
) -> MoteResult<Vec<EventEnvelope>> {
    let names = store.list_op_filenames()?;
    let selected = match after {
        Some(cursor) => {
            let cursor = cursor_filename(cursor)?;
            names
                .into_iter()
                .filter(|name| name.as_str() > cursor.as_str())
                .collect::<Vec<_>>()
        }
        None => names,
    };
    accepted_events_for_names(store, &selected, filter)
}

pub fn accepted_events_for_names(
    store: &Store,
    names: &[String],
    filter: &EventFilter,
) -> MoteResult<Vec<EventEnvelope>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
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
            out.push(event_from_op(&store_id, op)?);
        }
    }
    Ok(out)
}

pub fn state_for_names(store: &Store, names: &[String]) -> MoteResult<State> {
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        entries.push((name.clone(), std::fs::read(store.ops_dir().join(name))?));
    }
    Ok(reducer::replay(entries))
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

fn event_from_op(store_id: &str, op: Op) -> MoteResult<EventEnvelope> {
    let event_id = op.op_id().to_string();
    let event_type = event_type(&op).to_string();
    let category = event_category(&op).to_string();
    let op_id = op.op_id().to_string();
    let ts = op.ts().to_string();
    let actor = op.actor().to_string();
    let mut data = serde_json::to_value(&op)?;
    if let Some(obj) = data.as_object_mut() {
        for key in ["v", "op", "ts", "actor", "kind"] {
            obj.remove(key);
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
        Op::MsgSend(_) | Op::MsgAck(_) => "message",
        Op::BoardPost(_) | Op::BoardRead(_) | Op::BoardTopic(_) | Op::BoardSticky(_) => {
            "discussion"
        }
        Op::ReserveOpen(_) | Op::ReserveClose(_) => "reservation",
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
        Op::MsgSend(_) => "message.sent",
        Op::MsgAck(_) => "message.acknowledged",
        Op::BoardPost(_) => "discussion.posted",
        Op::BoardRead(_) => "discussion.read",
        Op::BoardTopic(_) => "discussion.topic_created",
        Op::BoardSticky(o) if o.sticky => "discussion.post_stuck",
        Op::BoardSticky(_) => "discussion.post_unstuck",
        Op::ReserveOpen(_) => "reservation.opened",
        Op::ReserveClose(_) => "reservation.closed",
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
    use crate::op::make_msg_send;
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
}
