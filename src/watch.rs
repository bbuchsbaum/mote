//! `mote watch`: stream snapshots of derived state whenever the op log changes.
//!
//! Read-only by construction: the implementation only ever calls
//! `reducer::replay_store`. Writes are produced by the regular CLI; this
//! command is a passive observer.

use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::Value;

use crate::errors::{MoteError, MoteResult};
use crate::ids;
use crate::reducer;
use crate::repo::Store;
use crate::state::State;

/// Run the watch loop. Returns only on a fatal error; SIGINT/Ctrl-C is the
/// normal way to exit.
pub fn run(
    store: &Store,
    actor: Option<&str>,
    json_mode: bool,
    interval_s: u64,
) -> MoteResult<i32> {
    let actor = actor.map(String::from);

    // Emit a starting snapshot before installing the watcher so a fresh viewer
    // always sees the current state, not just future deltas.
    emit_snapshot(store, actor.as_deref(), json_mode)?;

    let (tx, rx) = channel::<()>();

    // FS watcher on the ops/ directory. The reducer's contract is "filenames
    // in lex order"; new ops appear as Create events.
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

    // Periodic safety tick: re-emit every `interval_s` even if no FS event was
    // observed (network filesystems, sandboxed watchers, lease expiry).
    let tx_tick = tx.clone();
    let interval = Duration::from_secs(interval_s.max(1));
    thread::spawn(move || {
        loop {
            thread::sleep(interval);
            if tx_tick.send(()).is_err() {
                break;
            }
        }
    });

    loop {
        if rx.recv().is_err() {
            break;
        }
        drain_for(&rx, Duration::from_millis(150));
        emit_snapshot(store, actor.as_deref(), json_mode)?;
    }

    Ok(0)
}

/// Coalesce a burst of events into a single redraw.
fn drain_for(rx: &Receiver<()>, window: Duration) {
    let deadline = Instant::now() + window;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        if rx.recv_timeout(remaining).is_err() {
            return;
        }
    }
}

fn emit_snapshot(store: &Store, actor: Option<&str>, json_mode: bool) -> MoteResult<()> {
    let state = reducer::replay_store(store)?;
    let now = ids::format_rfc3339(Timestamp::now());
    if json_mode {
        let v = snapshot_value(&state, actor, &now);
        println!("{}", serde_json::to_string(&v)?);
    } else {
        print_human(&state, actor, &now);
    }
    Ok(())
}

/// JSON shape mirrors `mote board --json` plus an outer envelope with the
/// snapshot timestamp, so a consumer can tell two snapshots apart.
fn snapshot_value(state: &State, actor: Option<&str>, now: &str) -> Value {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for b in state.live_beads() {
        *counts.entry(b.status.as_str()).or_insert(0) += 1;
    }
    let active_claims: Vec<Value> = state
        .live_beads()
        .filter(|b| b.claim.as_ref().is_some_and(|c| c.is_live(now)))
        .map(|b| {
            serde_json::json!({
                "id": b.id,
                "title": b.title,
                "status": b.status.as_str(),
                "claimed_by": b.claim.as_ref().map(|c| &c.claimed_by),
                "lease_until_ts": b.claim.as_ref().map(|c| &c.lease_until_ts),
            })
        })
        .collect();
    let active_reservations: Vec<Value> = state
        .reservations
        .values()
        .filter(|r| r.is_active(now))
        .map(|r| {
            serde_json::json!({
                "reservation_id": r.reservation_id,
                "actor": r.actor,
                "entity": r.entity,
                "paths": r.live_paths(),
                "lease_until_ts": r.lease_until_ts,
            })
        })
        .collect();
    let inbox_unacked = actor.map(|a| state.inbox_for(a).len()).unwrap_or(0);
    let discussion_unread = actor
        .map(|a| state.unread_board_posts_for(a, None).len())
        .unwrap_or(0);

    serde_json::json!({
        "ts": now,
        "actor": actor,
        "status_counts": counts,
        "active_claims": active_claims,
        "active_reservations": active_reservations,
        "inbox_unacked": inbox_unacked,
        "discussion_unread": discussion_unread,
        "board_topics": state.board_topics.len(),
        "board_posts": state.board_posts.len(),
        "ops": state.history.values().map(Vec::len).sum::<usize>() + state.orphan_history.len(),
    })
}

fn print_human(state: &State, actor: Option<&str>, now: &str) {
    use std::collections::BTreeMap;

    // ANSI: clear screen + home cursor. We only emit this in human mode, so a
    // pipe to a file just gets the printed snapshots in order.
    print!("\x1b[2J\x1b[H");

    println!("mote watch  ({now})");
    if let Some(a) = actor {
        println!("actor:        {a}");
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for b in state.live_beads() {
        *counts.entry(b.status.as_str()).or_insert(0) += 1;
    }
    if counts.is_empty() {
        println!("status:       (no beads)");
    } else {
        let summary = counts
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("  ");
        println!("status:       {summary}");
    }

    let active_claims: Vec<_> = state
        .live_beads()
        .filter(|b| b.claim.as_ref().is_some_and(|c| c.is_live(now)))
        .collect();
    println!("claims:       {} active", active_claims.len());
    for b in active_claims.iter().take(10) {
        let holder = b
            .claim
            .as_ref()
            .map(|c| c.claimed_by.as_str())
            .unwrap_or("?");
        println!("  {} ({}, by {holder})", b.id, b.status.as_str());
    }
    if active_claims.len() > 10 {
        println!("  … {} more", active_claims.len() - 10);
    }

    let active_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.is_active(now))
        .collect();
    println!("reservations: {} active", active_reservations.len());
    for r in active_reservations.iter().take(10) {
        let live = r.live_paths().join(", ");
        println!(
            "  {} by {} on {}: {}",
            r.reservation_id, r.actor, r.entity, live
        );
    }
    if active_reservations.len() > 10 {
        println!("  … {} more", active_reservations.len() - 10);
    }

    let inbox = actor.map(|a| state.inbox_for(a).len()).unwrap_or(0);
    let unread = actor
        .map(|a| state.unread_board_posts_for(a, None).len())
        .unwrap_or(0);
    println!("inbox:        {inbox} unacked");
    println!("discussion:   {unread} unread");
    println!(
        "board:        {} topics, {} posts",
        state.board_topics.len(),
        state.board_posts.len()
    );

    let total_ops: usize =
        state.history.values().map(Vec::len).sum::<usize>() + state.orphan_history.len();
    println!("ops total:    {total_ops}");
}
