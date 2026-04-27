//! Pure reducer: replay a stream of (filename, bytes) into derived `State`.
//!
//! Replay is deterministic: filename ordering decides who wins on conflict,
//! per-field clocks are updated only on accepted ops, and rejected ops still
//! contribute to history with a reason string.

use std::collections::{BTreeMap, BTreeSet};

use crate::op::{
    ClaimOp, CloseOp, CreateOp, DeleteOp, DepOp, MsgAckOp, MsgSendOp, NoteOp, Op, PatchOp,
    ReleaseOp, ReserveCloseOp, ReserveOpenOp, ScalarSet, Status, TagOp, validate_msg_kind,
    validate_note_kind,
};
use crate::repo::Store;
use crate::state::{Bead, HistoryEntry, State};

/// Replay a sequence of ops, in the given filename order, into a fresh State.
pub fn replay<I>(ops: I) -> State
where
    I: IntoIterator<Item = (String, Vec<u8>)>,
{
    let mut state = State::default();
    for (filename, bytes) in ops {
        let op_id = filename
            .strip_suffix(".json")
            .unwrap_or(&filename)
            .to_string();
        match serde_json::from_slice::<Op>(&bytes) {
            Ok(op) => {
                if let Some(reason) = envelope_violation(&op_id, &filename, &op) {
                    let entity = op.entity().map(str::to_string);
                    state.push_history(
                        entity.as_deref(),
                        HistoryEntry::rejected(
                            &op_id,
                            op.kind_name(),
                            op.actor(),
                            op.ts(),
                            reason,
                        ),
                    );
                    continue;
                }
                apply(&mut state, &op_id, op);
            }
            Err(e) => {
                state.push_history(
                    None,
                    HistoryEntry::rejected(&op_id, "?", "?", "?", format!("malformed: {e}")),
                );
            }
        }
    }
    state
}

/// Verify the op envelope's `op` and `ts` fields agree with the filename.
/// Returns `Some(reason)` if they disagree, `None` if everything matches.
fn envelope_violation(op_id: &str, filename: &str, op: &Op) -> Option<String> {
    if op.op_id() != op_id {
        return Some(format!(
            "envelope op `{}` does not match filename stem `{op_id}`",
            op.op_id()
        ));
    }
    if let Ok(parts) = crate::ids::parse(filename) {
        match op.ts().parse::<jiff::Timestamp>() {
            Ok(parsed) => {
                let expected_basic = crate::ids::format_op_timestamp(parsed);
                if expected_basic != parts.ts {
                    return Some(format!(
                        "envelope ts `{}` does not match filename ts `{}`",
                        op.ts(),
                        parts.ts
                    ));
                }
            }
            Err(e) => {
                return Some(format!("envelope ts `{}` is unparseable: {e}", op.ts()));
            }
        }
    }
    None
}

/// Convenience wrapper that loads from a `Store`.
pub fn replay_store(store: &Store) -> crate::errors::MoteResult<State> {
    let names = store.list_op_filenames()?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
    for name in names {
        let bytes = std::fs::read(store.ops_dir().join(&name))?;
        entries.push((name, bytes));
    }
    Ok(replay(entries))
}

fn apply(state: &mut State, op_id: &str, op: Op) {
    let kind = op.kind_name();
    let actor = op.actor().to_string();
    let ts = op.ts().to_string();

    match op {
        Op::Create(o) => apply_create(state, op_id, kind, &actor, &ts, o),
        Op::Patch(o) => apply_patch(state, op_id, kind, &actor, &ts, o),
        Op::TagAdd(o) => apply_tag(state, op_id, kind, &actor, &ts, o, true),
        Op::TagRemove(o) => apply_tag(state, op_id, kind, &actor, &ts, o, false),
        Op::DepAdd(o) => apply_dep(state, op_id, kind, &actor, &ts, o, true),
        Op::DepRemove(o) => apply_dep(state, op_id, kind, &actor, &ts, o, false),
        Op::Note(o) => apply_note(state, op_id, kind, &actor, &ts, o),
        Op::Close(o) => apply_close(state, op_id, kind, &actor, &ts, o),
        Op::Delete(o) => apply_delete(state, op_id, kind, &actor, &ts, o),
        Op::Claim(o) => apply_claim(state, op_id, kind, &actor, &ts, o),
        Op::Release(o) => apply_release(state, op_id, kind, &actor, &ts, o),
        Op::MsgSend(o) => apply_msg_send(state, op_id, kind, &actor, &ts, o),
        Op::MsgAck(o) => apply_msg_ack(state, op_id, kind, &actor, &ts, o),
        Op::ReserveOpen(o) => apply_reserve_open(state, op_id, kind, &actor, &ts, o),
        Op::ReserveClose(o) => apply_reserve_close(state, op_id, kind, &actor, &ts, o),
    }
}

fn reject(state: &mut State, entity: &str, op_id: &str, kind: &str, actor: &str, ts: &str, reason: String) {
    state.push_history(Some(entity), HistoryEntry::rejected(op_id, kind, actor, ts, reason));
}

fn accept(state: &mut State, entity: &str, op_id: &str, kind: &str, actor: &str, ts: &str) {
    state.push_history(Some(entity), HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_create(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: CreateOp) {
    let CreateOp { entity, set, .. } = o;
    if state.beads.contains_key(&entity) {
        reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} already exists"));
        return;
    }
    let title = match &set.title {
        Some(t) if !t.is_empty() => t.clone(),
        _ => {
            reject(state, &entity, op_id, kind, actor, ts, "create requires non-empty title".into());
            return;
        }
    };
    if let Some(p) = set.priority {
        if !(0..=3).contains(&p) {
            reject(state, &entity, op_id, kind, actor, ts, format!("priority {p} out of 0..=3"));
            return;
        }
    }

    let mut clock = BTreeMap::new();
    for f in set.fields() {
        clock.insert(f.to_string(), op_id.to_string());
    }
    // Always set defaults on absent fields, with their clock = op_id (so a
    // future patch can target them with `expect.<field> = create_op_id`).
    for f in &["title", "status", "priority", "body"] {
        clock.entry((*f).to_string()).or_insert_with(|| op_id.to_string());
    }
    if set.assignee.is_some() {
        clock.insert("assignee".to_string(), op_id.to_string());
    }

    let bead = Bead {
        id: entity.clone(),
        title,
        status: set.status.unwrap_or(Status::Open),
        priority: set.priority.unwrap_or(2),
        body: set.body.unwrap_or_default(),
        assignee: set.assignee,
        tags: BTreeSet::new(),
        deps: BTreeSet::new(),
        clock,
        notes: Vec::new(),
        claim: None,
        created_at_op: op_id.to_string(),
        created_at_ts: ts.to_string(),
        deleted_at_ts: None,
    };
    state.beads.insert(entity.clone(), bead);
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_patch(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: PatchOp) {
    let PatchOp { entity, expect, set, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} is deleted"));
            return;
        }
        None => {
            reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist"));
            return;
        }
    };

    // expect check: every field in expect must match current clock.
    for (field, expected) in &expect {
        match bead.clock.get(field) {
            Some(current) if current == expected => {}
            Some(current) => {
                let reason = format!(
                    "stale: field `{field}` clock is {current}, expected {expected}"
                );
                reject(state, &entity, op_id, kind, actor, ts, reason);
                return;
            }
            None => {
                let reason = format!("stale: field `{field}` has never been written, expected {expected}");
                reject(state, &entity, op_id, kind, actor, ts, reason);
                return;
            }
        }
    }

    if let Some(p) = set.priority {
        if !(0..=3).contains(&p) {
            reject(state, &entity, op_id, kind, actor, ts, format!("priority {p} out of 0..=3"));
            return;
        }
    }

    apply_scalar_set(bead, op_id, &set);
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_scalar_set(bead: &mut Bead, op_id: &str, set: &ScalarSet) {
    if let Some(t) = &set.title {
        bead.title = t.clone();
        bead.clock.insert("title".into(), op_id.into());
    }
    if let Some(s) = set.status {
        bead.status = s;
        bead.clock.insert("status".into(), op_id.into());
    }
    if let Some(p) = set.priority {
        bead.priority = p;
        bead.clock.insert("priority".into(), op_id.into());
    }
    if let Some(b) = &set.body {
        bead.body = b.clone();
        bead.clock.insert("body".into(), op_id.into());
    }
    if let Some(a) = &set.assignee {
        bead.assignee = Some(a.clone());
        bead.clock.insert("assignee".into(), op_id.into());
    }
}

fn apply_tag(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: TagOp, add: bool) {
    let TagOp { entity, tag, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => { reject(state, &entity, op_id, kind, actor, ts, "entity is deleted".into()); return; }
        None => { reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist")); return; }
    };
    if add { bead.tags.insert(tag); } else { bead.tags.remove(&tag); }
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_dep(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: DepOp, add: bool) {
    let DepOp { entity, parent, dep_kind, .. } = o;
    if entity == parent {
        reject(state, &entity, op_id, kind, actor, ts, "self-dependency forbidden".into());
        return;
    }
    // For `dep_add` the parent must exist AND not be deleted (we can't add a
    // dependency on a tombstoned parent). For `dep_remove` we only require
    // the parent to be a known entity — removal should still be idempotent
    // even after the parent has been deleted.
    let parent_present = state.beads.contains_key(&parent);
    if !parent_present {
        reject(state, &entity, op_id, kind, actor, ts, format!("parent {parent} does not exist"));
        return;
    }
    if add {
        let parent_alive = state
            .beads
            .get(&parent)
            .map_or(false, |b| !b.is_deleted());
        if !parent_alive {
            reject(state, &entity, op_id, kind, actor, ts, format!("parent {parent} is deleted"));
            return;
        }
    }
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => { reject(state, &entity, op_id, kind, actor, ts, "entity is deleted".into()); return; }
        None => { reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist")); return; }
    };
    let edge = (parent, dep_kind);
    if add { bead.deps.insert(edge); } else { bead.deps.remove(&edge); }
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_note(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: NoteOp) {
    let NoteOp { entity, note_kind, text, .. } = o;
    if !validate_note_kind(&note_kind) {
        reject(state, &entity, op_id, kind, actor, ts, format!("invalid note_kind: {note_kind}"));
        return;
    }
    let bead = match state.beads.get_mut(&entity) {
        Some(b) => b,
        None => {
            reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist"));
            return;
        }
    };
    bead.notes.push(crate::state::Note {
        op_id: op_id.to_string(),
        note_kind,
        actor: actor.to_string(),
        ts: ts.to_string(),
        text,
    });
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_close(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: CloseOp) {
    let CloseOp { entity, expect, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => { reject(state, &entity, op_id, kind, actor, ts, "entity is deleted".into()); return; }
        None => { reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist")); return; }
    };
    if bead.status == Status::Closed {
        // Idempotent no-op.
        accept(state, &entity, op_id, kind, actor, ts);
        return;
    }
    for (field, expected) in &expect {
        match bead.clock.get(field) {
            Some(current) if current == expected => {}
            Some(current) => {
                let reason = format!("stale close: `{field}` clock is {current}, expected {expected}");
                reject(state, &entity, op_id, kind, actor, ts, reason);
                return;
            }
            None => {
                reject(state, &entity, op_id, kind, actor, ts, format!("stale close: `{field}` never written"));
                return;
            }
        }
    }
    bead.status = Status::Closed;
    bead.clock.insert("status".into(), op_id.into());
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_claim(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: ClaimOp) {
    let ClaimOp {
        entity,
        to,
        ttl_s,
        expect_claim,
        ..
    } = o;

    // Inspect-only first to avoid mutable-borrow conflict with `reject(state, ...)`.
    enum Decision {
        Accept,
        EntityDeleted,
        EntityMissing,
        Held(String),
    }
    let decision = match state.beads.get(&entity) {
        Some(b) if b.is_deleted() => Decision::EntityDeleted,
        Some(b) => match (&b.claim, expect_claim.as_deref()) {
            (None, _) => Decision::Accept,
            (Some(c), Some(ec)) if ec == c.claim_clock => Decision::Accept,
            (Some(c), _) if !c.is_live(ts) => Decision::Accept,
            (Some(c), _) => Decision::Held(c.claimed_by.clone()),
        },
        None => Decision::EntityMissing,
    };

    match decision {
        Decision::EntityDeleted => {
            reject(state, &entity, op_id, kind, actor, ts, "entity is deleted".into());
            return;
        }
        Decision::EntityMissing => {
            reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist"));
            return;
        }
        Decision::Held(holder) => {
            reject(state, &entity, op_id, kind, actor, ts, format!("claim still held by {holder}"));
            return;
        }
        Decision::Accept => {}
    }

    let lease_until_ts = match compute_lease_until(ts, ttl_s) {
        Ok(s) => s,
        Err(e) => {
            reject(state, &entity, op_id, kind, actor, ts, format!("bad ttl: {e}"));
            return;
        }
    };

    let bead = state.beads.get_mut(&entity).expect("checked above");
    bead.claim = Some(crate::state::ClaimState {
        claimed_by: to,
        claim_clock: op_id.to_string(),
        lease_until_ts,
    });
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_release(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: ReleaseOp) {
    let ReleaseOp { entity, expect_claim, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(state, &entity, op_id, kind, actor, ts, "entity is deleted".into());
            return;
        }
        None => {
            reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist"));
            return;
        }
    };

    let claim = match &bead.claim {
        Some(c) => c.clone(),
        None => {
            reject(state, &entity, op_id, kind, actor, ts, "no active claim to release".into());
            return;
        }
    };

    let by_holder = claim.claimed_by == actor;
    let by_expect = matches!(expect_claim.as_deref(), Some(ec) if ec == claim.claim_clock);
    if !by_holder && !by_expect {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("release rejected: held by {}, expect_claim mismatch", claim.claimed_by),
        );
        return;
    }

    bead.claim = None;
    accept(state, &entity, op_id, kind, actor, ts);
}

fn compute_lease_until(ts: &str, ttl_s: u32) -> Result<String, String> {
    let parsed: jiff::Timestamp = ts.parse().map_err(|e: jiff::Error| e.to_string())?;
    let dur = jiff::SignedDuration::from_secs(ttl_s as i64);
    let lease_until = parsed.checked_add(dur).map_err(|e| e.to_string())?;
    Ok(crate::ids::format_rfc3339(lease_until))
}

fn apply_msg_send(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: MsgSendOp) {
    let MsgSendOp {
        msg_id,
        to,
        entity,
        reservation,
        msg_kind,
        body,
        ..
    } = o;

    let bind_entity = entity.as_deref();

    if !validate_msg_kind(&msg_kind) {
        if let Some(e) = bind_entity {
            reject(state, e, op_id, kind, actor, ts, format!("invalid msg_kind: {msg_kind}"));
        } else {
            state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, format!("invalid msg_kind: {msg_kind}")));
        }
        return;
    }
    if state.messages.contains_key(&msg_id) {
        let reason = format!("duplicate msg_id {msg_id}");
        if let Some(e) = bind_entity {
            reject(state, e, op_id, kind, actor, ts, reason);
        } else {
            state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason));
        }
        return;
    }
    if let Some(e) = bind_entity {
        if !state.beads.contains_key(e) {
            reject(state, e, op_id, kind, actor, ts, format!("entity {e} does not exist"));
            return;
        }
    }

    state.messages.insert(
        msg_id.clone(),
        crate::state::MsgRecord {
            msg_id,
            from: actor.to_string(),
            to,
            entity: entity.clone(),
            reservation,
            msg_kind,
            body,
            sent_ts: ts.to_string(),
            sent_op_id: op_id.to_string(),
            ack_op_id: None,
            ack_ts: None,
        },
    );

    state.push_history(bind_entity, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_msg_ack(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: MsgAckOp) {
    let MsgAckOp { msg_id, .. } = o;

    let msg = match state.messages.get_mut(&msg_id) {
        Some(m) => m,
        None => {
            state.push_history(
                None,
                HistoryEntry::rejected(op_id, kind, actor, ts, format!("no such msg_id {msg_id}")),
            );
            return;
        }
    };

    let bind_entity = msg.entity.clone();

    if msg.from == actor {
        let reason = "self-ack of own send is not allowed".to_string();
        match bind_entity.as_deref() {
            Some(e) => reject(state, e, op_id, kind, actor, ts, reason),
            None => state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason)),
        }
        return;
    }
    if msg.to != actor {
        let reason = format!("msg {msg_id} addressed to {}, not {}", msg.to, actor);
        match bind_entity.as_deref() {
            Some(e) => reject(state, e, op_id, kind, actor, ts, reason),
            None => state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason)),
        }
        return;
    }
    if msg.ack_op_id.is_some() {
        let reason = format!("msg {msg_id} already acked");
        match bind_entity.as_deref() {
            Some(e) => reject(state, e, op_id, kind, actor, ts, reason),
            None => state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason)),
        }
        return;
    }

    msg.ack_op_id = Some(op_id.to_string());
    msg.ack_ts = Some(ts.to_string());

    state.push_history(bind_entity.as_deref(), HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_reserve_open(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: ReserveOpenOp,
) {
    let ReserveOpenOp {
        reservation_id,
        entity,
        paths,
        ttl_s,
        mode,
        ..
    } = o;

    if mode != "exclusive" {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("mode `{mode}` not supported in v0.2 (only `exclusive`)"),
        );
        return;
    }
    if state.reservations.contains_key(&reservation_id) {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("reservation_id `{reservation_id}` already exists"),
        );
        return;
    }
    if !state.beads.get(&entity).map_or(false, |b| !b.is_deleted()) {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("entity {entity} does not exist"),
        );
        return;
    }
    if paths.is_empty() {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            "reserve_open requires at least one path".into(),
        );
        return;
    }

    let mut normalized = Vec::with_capacity(paths.len());
    for p in &paths {
        match crate::paths::normalize(p) {
            Ok(n) => normalized.push(n),
            Err(e) => {
                reject(
                    state,
                    &entity,
                    op_id,
                    kind,
                    actor,
                    ts,
                    format!("path `{p}`: {e}"),
                );
                return;
            }
        }
    }

    if let Some(reason) = first_overlap_reason(state, actor, ts, &normalized) {
        reject(state, &entity, op_id, kind, actor, ts, reason);
        return;
    }

    let lease_until_ts = match compute_lease_until(ts, ttl_s) {
        Ok(s) => s,
        Err(e) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("bad ttl: {e}"),
            );
            return;
        }
    };

    state.reservations.insert(
        reservation_id.clone(),
        crate::state::ReservationState {
            reservation_id,
            actor: actor.to_string(),
            entity: entity.clone(),
            paths: normalized,
            ttl_s,
            opened_op_id: op_id.to_string(),
            opened_ts: ts.to_string(),
            lease_until_ts,
            closed_paths: std::collections::BTreeSet::new(),
        },
    );
    accept(state, &entity, op_id, kind, actor, ts);
}

fn first_overlap_reason(
    state: &State,
    actor: &str,
    ts: &str,
    new_paths: &[String],
) -> Option<String> {
    for r in state.reservations.values() {
        if r.actor == actor || !r.is_live(ts) {
            continue;
        }
        for p_new in new_paths {
            for p_held in r.paths.iter().filter(|p| !r.closed_paths.contains(p.as_str())) {
                if crate::paths::overlap(p_new, p_held) {
                    return Some(format!(
                        "path conflict: `{p_new}` overlaps `{p_held}` held by {} (rv {})",
                        r.actor, r.reservation_id
                    ));
                }
            }
        }
    }
    None
}

fn apply_reserve_close(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: ReserveCloseOp,
) {
    let ReserveCloseOp {
        reservation_id,
        paths,
        ..
    } = o;
    let (entity, owner, all_paths) = match state.reservations.get(&reservation_id) {
        Some(r) => (r.entity.clone(), r.actor.clone(), r.paths.clone()),
        None => {
            state.push_history(
                None,
                HistoryEntry::rejected(
                    op_id,
                    kind,
                    actor,
                    ts,
                    format!("no such reservation `{reservation_id}`"),
                ),
            );
            return;
        }
    };
    if owner != actor {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("reservation `{reservation_id}` is owned by {owner}"),
        );
        return;
    }
    let to_close: Vec<String> = match paths {
        Some(p) if !p.is_empty() => {
            // Normalize each requested path and verify it belongs to this
            // reservation. Reject the whole op on any failure so the user gets
            // a clear error rather than a silent no-op.
            let mut normalized = Vec::with_capacity(p.len());
            for raw in &p {
                let n = match crate::paths::normalize(raw) {
                    Ok(n) => n,
                    Err(e) => {
                        reject(
                            state,
                            &entity,
                            op_id,
                            kind,
                            actor,
                            ts,
                            format!("path `{raw}`: {e}"),
                        );
                        return;
                    }
                };
                if !all_paths.iter().any(|ap| ap == &n) {
                    reject(
                        state,
                        &entity,
                        op_id,
                        kind,
                        actor,
                        ts,
                        format!("path `{n}` is not in reservation `{reservation_id}`"),
                    );
                    return;
                }
                normalized.push(n);
            }
            normalized
        }
        _ => all_paths,
    };
    let r = state
        .reservations
        .get_mut(&reservation_id)
        .expect("checked above");
    for p in &to_close {
        r.closed_paths.insert(p.clone());
    }
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_delete(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: DeleteOp) {
    let DeleteOp { entity, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => { reject(state, &entity, op_id, kind, actor, ts, "entity already deleted".into()); return; }
        None => { reject(state, &entity, op_id, kind, actor, ts, format!("entity {entity} does not exist")); return; }
    };
    bead.deleted_at_ts = Some(ts.to_string());
    accept(state, &entity, op_id, kind, actor, ts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical;
    use crate::ids;
    use crate::op::{make_create, make_dep, make_patch, make_tag};
    use serde_json::Value;

    fn pack(op: Op) -> (String, Vec<u8>) {
        // Serialize, build name, set op field, re-serialize.
        let ts: jiff::Timestamp = op.ts().parse().unwrap();
        let mut value: Value = serde_json::to_value(&op).unwrap();
        value.as_object_mut().unwrap().insert("op".into(), Value::String(String::new()));
        let bytes_for_hash = canonical::encode(&value);
        let name = ids::build_op_name(ts, &bytes_for_hash);
        value.as_object_mut().unwrap().insert("op".into(), Value::String(name.as_str().into()));
        let bytes = canonical::encode(&value);
        (format!("{}.json", name.as_str()), bytes)
    }

    fn ts_at(s: &str) -> jiff::Timestamp { s.parse().unwrap() }

    #[test]
    fn create_then_patch_disjoint_fields_succeeds() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");

        let create = make_create("alice".into(), "bd-1".into(),
            ScalarSet { title: Some("hi".into()), ..Default::default() }, t1);
        let (n1, b1) = pack(create);

        // Need create_op_id to construct expect for patch.
        let create_op_id = n1.strip_suffix(".json").unwrap().to_string();

        let mut expect = BTreeMap::new();
        expect.insert("priority".into(), create_op_id.clone());
        let patch = make_patch("bob".into(), "bd-1".into(), expect,
            ScalarSet { priority: Some(1), ..Default::default() }, t2);
        let (n2, b2) = pack(patch);

        let state = replay([(n1.clone(), b1), (n2.clone(), b2)]);
        let bead = &state.beads["bd-1"];
        assert_eq!(bead.title, "hi");
        assert_eq!(bead.priority, 1);
    }

    #[test]
    fn patch_same_field_race_only_one_accepted() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");
        let t3 = ts_at("2026-04-20T18:24:55.000003Z");

        let create = make_create("alice".into(), "bd-1".into(),
            ScalarSet { title: Some("t".into()), ..Default::default() }, t1);
        let (n1, b1) = pack(create);
        let create_op_id = n1.strip_suffix(".json").unwrap().to_string();

        let mut expect = BTreeMap::new();
        expect.insert("status".into(), create_op_id);
        let p1 = make_patch("a".into(), "bd-1".into(), expect.clone(),
            ScalarSet { status: Some(Status::Doing), ..Default::default() }, t2);
        let p2 = make_patch("b".into(), "bd-1".into(), expect,
            ScalarSet { status: Some(Status::Blocked), ..Default::default() }, t3);
        let (n2, b2) = pack(p1);
        let (n3, b3) = pack(p2);

        let state = replay([(n1, b1), (n2.clone(), b2), (n3.clone(), b3)]);
        let bead = &state.beads["bd-1"];
        // First wins (lex-earlier filename); second is rejected as stale.
        assert_eq!(bead.status, Status::Doing);

        let id2 = n2.strip_suffix(".json").unwrap();
        let id3 = n3.strip_suffix(".json").unwrap();
        assert!(state.was_accepted(id2));
        assert!(!state.was_accepted(id3));
        let reason = state.rejection_reason(id3).expect("rejected with reason");
        assert!(reason.contains("stale"));
    }

    #[test]
    fn dep_self_dependency_rejected() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");
        let create = make_create("a".into(), "bd-1".into(),
            ScalarSet { title: Some("t".into()), ..Default::default() }, t1);
        let dep = make_dep(true, "a".into(), "bd-1".into(), "bd-1".into(), "blocks".into(), t2);
        let (n1, b1) = pack(create);
        let (n2, b2) = pack(dep);
        let state = replay([(n1, b1), (n2.clone(), b2)]);
        let id = n2.strip_suffix(".json").unwrap();
        assert!(!state.was_accepted(id));
        let reason = state.rejection_reason(id).unwrap_or_default();
        assert!(reason.contains("self"), "expected reason about self-dep, got: {reason:?}");
    }

    #[test]
    fn tag_add_idempotent() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");
        let t3 = ts_at("2026-04-20T18:24:55.000003Z");
        let create = make_create("a".into(), "bd-1".into(),
            ScalarSet { title: Some("t".into()), ..Default::default() }, t1);
        let tag1 = make_tag(true, "a".into(), "bd-1".into(), "x".into(), t2);
        let tag2 = make_tag(true, "a".into(), "bd-1".into(), "x".into(), t3);
        let (n1, b1) = pack(create);
        let (n2, b2) = pack(tag1);
        let (n3, b3) = pack(tag2);
        let state = replay([(n1, b1), (n2, b2), (n3, b3)]);
        let bead = &state.beads["bd-1"];
        assert_eq!(bead.tags.len(), 1);
        assert!(bead.tags.contains("x"));
    }

    #[test]
    fn replay_is_deterministic() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");
        let create = make_create("a".into(), "bd-1".into(),
            ScalarSet { title: Some("t".into()), ..Default::default() }, t1);
        let (n1, b1) = pack(create);
        let create_op_id = n1.strip_suffix(".json").unwrap().to_string();
        let mut expect = BTreeMap::new();
        expect.insert("title".into(), create_op_id);
        let patch = make_patch("a".into(), "bd-1".into(), expect,
            ScalarSet { title: Some("t2".into()), ..Default::default() }, t2);
        let (n2, b2) = pack(patch);

        let s1 = replay([(n1.clone(), b1.clone()), (n2.clone(), b2.clone())]);
        let s2 = replay([(n1, b1), (n2, b2)]);
        assert_eq!(s1.beads["bd-1"].title, s2.beads["bd-1"].title);
        assert_eq!(s1.beads["bd-1"].clock, s2.beads["bd-1"].clock);
    }
}
