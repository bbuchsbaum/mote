//! Pure reducer: replay a stream of (filename, bytes) into derived `State`.
//!
//! Replay is deterministic: filename ordering decides who wins on conflict,
//! per-field clocks are updated only on accepted ops, and rejected ops still
//! contribute to history with a reason string.

use std::collections::{BTreeMap, BTreeSet};

use crate::op::{
    BoardPostOp, BoardReadOp, BoardRetractOp, BoardRouteOp, BoardStickyOp, BoardSupersedeOp,
    BoardTopicOp, BoardWatchOp, CandidateAbandonOp, CandidateAuthorizeOp, CandidateEvidenceOp,
    CandidateLandedOp, CandidateProposeOp, CandidateReviewOp, CandidateRevokeOp,
    CandidateSupersedeOp, ClaimOp, CloseOp, CreateOp, DeleteOp, DepOp, MsgAckOp, MsgResolveOp,
    MsgSendOp, NoteOp, Op, PatchOp, RelOp, ReleaseOp, ReserveAdoptOp, ReserveCloseOp,
    ReserveOpenOp, ScalarSet, SessionEndOp, SessionHeartbeatOp, SessionStartOp, SessionStatusOp,
    Status, TagOp, VALID_POST_KINDS, VALID_REPLY_KINDS, VALID_ROUTE_STATES,
    validate_idempotency_key, validate_msg_kind, validate_note_kind, validate_post_kind,
    validate_route_state, validate_session_intent,
};
use crate::repo::Store;
use crate::state::{Bead, HistoryEntry, RequestState, RouteState, State};

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
                        HistoryEntry::rejected(&op_id, op.kind_name(), op.actor(), op.ts(), reason),
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
    let candidate_retry = op.candidate_idempotency().map(|(candidate_id, key)| {
        (
            candidate_id.to_string(),
            key.to_string(),
            crate::candidate::action_digest(&op),
        )
    });
    let session_retry = match &op {
        Op::SessionHeartbeat(o) => o.idempotency_key.as_deref(),
        Op::SessionStatus(o) => o.idempotency_key.as_deref(),
        _ => None,
    }
    .map(|key| {
        (
            key.to_string(),
            crate::op::session_action_digest(&op).expect("session retry op has a digest"),
        )
    });
    if let Some((key, digest)) = &session_retry {
        if !validate_idempotency_key(key) {
            reject_orphan(
                state,
                op_id,
                kind,
                &actor,
                &ts,
                "idempotency_key must be 1..=128 trimmed printable characters".into(),
            );
            return;
        }
        if let Some(previous) = state.session_idempotency.get(&(actor.clone(), key.clone())) {
            let reason = if previous.digest == *digest && previous.kind == kind {
                format!("idempotent retry already accepted as op {}", previous.op_id)
            } else {
                format!(
                    "idempotency key `{key}` already used by op {} for a different session action",
                    previous.op_id
                )
            };
            reject_orphan(state, op_id, kind, &actor, &ts, reason);
            return;
        }
    }
    if let Some((candidate_id, key, digest)) = &candidate_retry {
        if !validate_idempotency_key(key) {
            reject(
                state,
                candidate_id,
                op_id,
                kind,
                &actor,
                &ts,
                "invalid idempotency key".into(),
            );
            return;
        }
        let digest = match digest {
            Ok(digest) => digest,
            Err(error) => {
                reject(
                    state,
                    candidate_id,
                    op_id,
                    kind,
                    &actor,
                    &ts,
                    format!("cannot digest candidate action: {error}"),
                );
                return;
            }
        };
        if let Some(previous) = state
            .candidate_idempotency
            .get(&(actor.clone(), key.clone()))
        {
            if previous.candidate_id == *candidate_id && previous.digest == *digest {
                accept(state, candidate_id, op_id, kind, &actor, &ts);
            } else {
                reject(
                    state,
                    candidate_id,
                    op_id,
                    kind,
                    &actor,
                    &ts,
                    format!(
                        "idempotency key already used by op {} for a different action",
                        previous.op_id
                    ),
                );
            }
            return;
        }
    }

    match op {
        Op::Create(o) => apply_create(state, op_id, kind, &actor, &ts, o),
        Op::Patch(o) => apply_patch(state, op_id, kind, &actor, &ts, o),
        Op::TagAdd(o) => apply_tag(state, op_id, kind, &actor, &ts, o, true),
        Op::TagRemove(o) => apply_tag(state, op_id, kind, &actor, &ts, o, false),
        Op::DepAdd(o) => apply_dep(state, op_id, kind, &actor, &ts, o, true),
        Op::DepRemove(o) => apply_dep(state, op_id, kind, &actor, &ts, o, false),
        Op::RelAdd(o) => apply_rel(state, op_id, kind, &actor, &ts, o, true),
        Op::RelRemove(o) => apply_rel(state, op_id, kind, &actor, &ts, o, false),
        Op::Note(o) => apply_note(state, op_id, kind, &actor, &ts, o),
        Op::Close(o) => apply_close(state, op_id, kind, &actor, &ts, o),
        Op::Delete(o) => apply_delete(state, op_id, kind, &actor, &ts, o),
        Op::Claim(o) => apply_claim(state, op_id, kind, &actor, &ts, o),
        Op::Release(o) => apply_release(state, op_id, kind, &actor, &ts, o),
        Op::MsgSend(o) => apply_msg_send(state, op_id, kind, &actor, &ts, o),
        Op::MsgAck(o) => apply_msg_ack(state, op_id, kind, &actor, &ts, o),
        Op::MsgResolve(o) => apply_msg_resolve(state, op_id, kind, &actor, &ts, o),
        Op::BoardPost(o) => apply_board_post(state, op_id, kind, &actor, &ts, o),
        Op::BoardRead(o) => apply_board_read(state, op_id, kind, &actor, &ts, o),
        Op::BoardWatch(o) => apply_board_watch(state, op_id, kind, &actor, &ts, o),
        Op::BoardTopic(o) => apply_board_topic(state, op_id, kind, &actor, &ts, o),
        Op::BoardSticky(o) => apply_board_sticky(state, op_id, kind, &actor, &ts, o),
        Op::BoardSupersede(o) => apply_board_supersede(state, op_id, kind, &actor, &ts, o),
        Op::BoardRetract(o) => apply_board_retract(state, op_id, kind, &actor, &ts, o),
        Op::BoardRoute(o) => apply_board_route(state, op_id, kind, &actor, &ts, o),
        Op::SessionStart(o) => apply_session_start(state, op_id, kind, &actor, &ts, o),
        Op::SessionHeartbeat(o) => apply_session_heartbeat(state, op_id, kind, &actor, &ts, o),
        Op::SessionStatus(o) => apply_session_status(state, op_id, kind, &actor, &ts, o),
        Op::SessionEnd(o) => apply_session_end(state, op_id, kind, &actor, &ts, o),
        Op::ReserveOpen(o) => apply_reserve_open(state, op_id, kind, &actor, &ts, o),
        Op::ReserveClose(o) => apply_reserve_close(state, op_id, kind, &actor, &ts, o),
        Op::ReserveAdopt(o) => apply_reserve_adopt(state, op_id, kind, &actor, &ts, o),
        Op::CandidatePropose(o) => apply_candidate_propose(state, op_id, kind, &actor, &ts, o),
        Op::CandidateEvidence(o) => apply_candidate_evidence(state, op_id, kind, &actor, &ts, o),
        Op::CandidateReview(o) => apply_candidate_review(state, op_id, kind, &actor, &ts, o),
        Op::CandidateAuthorize(o) => apply_candidate_authorize(state, op_id, kind, &actor, &ts, o),
        Op::CandidateRevoke(o) => apply_candidate_revoke(state, op_id, kind, &actor, &ts, o),
        Op::CandidateSupersede(o) => apply_candidate_supersede(state, op_id, kind, &actor, &ts, o),
        Op::CandidateAbandon(o) => apply_candidate_abandon(state, op_id, kind, &actor, &ts, o),
        Op::CandidateLanded(o) => apply_candidate_landed(state, op_id, kind, &actor, &ts, o),
    }

    if let Some((candidate_id, key, Ok(digest))) = candidate_retry {
        if state.was_accepted(op_id) {
            state.candidate_idempotency.insert(
                (actor.clone(), key),
                crate::state::CandidateIdempotencyRecord {
                    candidate_id,
                    digest,
                    op_id: op_id.to_string(),
                },
            );
        }
    }
    if let Some((key, digest)) = session_retry {
        if state.was_accepted(op_id) {
            state.session_idempotency.insert(
                (actor, key),
                crate::state::SessionIdempotencyRecord {
                    op_id: op_id.to_string(),
                    kind: kind.to_string(),
                    digest,
                },
            );
        }
    }
}

fn reject(
    state: &mut State,
    entity: &str,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    reason: String,
) {
    state.push_history(
        Some(entity),
        HistoryEntry::rejected(op_id, kind, actor, ts, reason),
    );
}

fn accept(state: &mut State, entity: &str, op_id: &str, kind: &str, actor: &str, ts: &str) {
    state.push_history(Some(entity), HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_create(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: CreateOp) {
    let CreateOp { entity, set, .. } = o;
    if state.beads.contains_key(&entity) {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("entity {entity} already exists"),
        );
        return;
    }
    let title = match &set.title {
        Some(t) if !t.is_empty() => t.clone(),
        _ => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "create requires non-empty title".into(),
            );
            return;
        }
    };
    if let Some(p) = set.priority {
        if !(0..=3).contains(&p) {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("priority {p} out of 0..=3"),
            );
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
        clock
            .entry((*f).to_string())
            .or_insert_with(|| op_id.to_string());
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
        rels: BTreeSet::new(),
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
    let PatchOp {
        entity,
        expect,
        set,
        ..
    } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("entity {entity} is deleted"),
            );
            return;
        }
        None => {
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
    };

    // expect check: every field in expect must match current clock.
    for (field, expected) in &expect {
        match bead.clock.get(field) {
            Some(current) if current == expected => {}
            Some(current) => {
                let reason =
                    format!("stale: field `{field}` clock is {current}, expected {expected}");
                reject(state, &entity, op_id, kind, actor, ts, reason);
                return;
            }
            None => {
                let reason =
                    format!("stale: field `{field}` has never been written, expected {expected}");
                reject(state, &entity, op_id, kind, actor, ts, reason);
                return;
            }
        }
    }

    if let Some(p) = set.priority {
        if !(0..=3).contains(&p) {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("priority {p} out of 0..=3"),
            );
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

fn apply_tag(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: TagOp,
    add: bool,
) {
    let TagOp { entity, tag, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "entity is deleted".into(),
            );
            return;
        }
        None => {
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
    };
    if add {
        bead.tags.insert(tag);
    } else {
        bead.tags.remove(&tag);
    }
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_dep(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: DepOp,
    add: bool,
) {
    let DepOp {
        entity,
        parent,
        dep_kind,
        ..
    } = o;
    if entity == parent {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            "self-dependency forbidden".into(),
        );
        return;
    }
    // For `dep_add` the parent must exist AND not be deleted (we can't add a
    // dependency on a tombstoned parent). For `dep_remove` we only require
    // the parent to be a known entity — removal should still be idempotent
    // even after the parent has been deleted.
    let parent_present = state.beads.contains_key(&parent);
    if !parent_present {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("parent {parent} does not exist"),
        );
        return;
    }
    if add {
        let parent_alive = state.beads.get(&parent).is_some_and(|b| !b.is_deleted());
        if !parent_alive {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("parent {parent} is deleted"),
            );
            return;
        }
    }
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "entity is deleted".into(),
            );
            return;
        }
        None => {
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
    };
    let edge = (parent, dep_kind);
    if add {
        bead.deps.insert(edge);
    } else {
        bead.deps.remove(&edge);
    }
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_rel(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: RelOp,
    add: bool,
) {
    let RelOp {
        entity,
        parent,
        rel_kind,
        ..
    } = o;
    if entity == parent {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            "self-relation forbidden".into(),
        );
        return;
    }

    let parent_present = state.beads.contains_key(&parent);
    if !parent_present {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("parent {parent} does not exist"),
        );
        return;
    }
    if add {
        let parent_alive = state.beads.get(&parent).is_some_and(|b| !b.is_deleted());
        if !parent_alive {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("parent {parent} is deleted"),
            );
            return;
        }
    }

    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "entity is deleted".into(),
            );
            return;
        }
        None => {
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
    };
    let edge = (parent, rel_kind);
    if add {
        bead.rels.insert(edge);
    } else {
        bead.rels.remove(&edge);
    }
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_note(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: NoteOp) {
    let NoteOp {
        entity,
        note_kind,
        text,
        ..
    } = o;
    if !validate_note_kind(&note_kind) {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("invalid note_kind: {note_kind}"),
        );
        return;
    }
    let bead = match state.beads.get_mut(&entity) {
        Some(b) => b,
        None => {
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
        Some(_) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "entity is deleted".into(),
            );
            return;
        }
        None => {
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
                let reason =
                    format!("stale close: `{field}` clock is {current}, expected {expected}");
                reject(state, &entity, op_id, kind, actor, ts, reason);
                return;
            }
            None => {
                reject(
                    state,
                    &entity,
                    op_id,
                    kind,
                    actor,
                    ts,
                    format!("stale close: `{field}` never written"),
                );
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
        EntityClosed,
        EntityMissing,
        Held(String),
    }
    let decision = match state.beads.get(&entity) {
        Some(b) if b.is_deleted() => Decision::EntityDeleted,
        Some(b) if b.status == Status::Closed => Decision::EntityClosed,
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
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "entity is deleted".into(),
            );
            return;
        }
        Decision::EntityMissing => {
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
        Decision::EntityClosed => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "cannot claim or renew closed work".into(),
            );
            return;
        }
        Decision::Held(holder) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("claim still held by {holder}"),
            );
            return;
        }
        Decision::Accept => {}
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

    let bead = state.beads.get_mut(&entity).expect("checked above");
    bead.claim = Some(crate::state::ClaimState {
        claimed_by: to,
        claim_clock: op_id.to_string(),
        lease_until_ts,
    });
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_release(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: ReleaseOp) {
    let ReleaseOp {
        entity,
        expect_claim,
        ..
    } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) => b,
        None => {
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
    };

    let claim = match &bead.claim {
        Some(c) => c.clone(),
        None => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "no active claim to release".into(),
            );
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
            format!(
                "release rejected: held by {}, expect_claim mismatch",
                claim.claimed_by
            ),
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

fn reject_message(
    state: &mut State,
    entity: Option<&str>,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    reason: String,
) {
    match entity {
        Some(entity) => reject(state, entity, op_id, kind, actor, ts, reason),
        None => state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason)),
    }
}

fn apply_msg_send(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: MsgSendOp) {
    let MsgSendOp {
        msg_id,
        to,
        mut entity,
        mut reservation,
        msg_kind,
        body,
        reply_to,
        mut correlation_id,
        idempotency_key,
        answers,
        require_live,
        ..
    } = o;

    if !validate_msg_kind(&msg_kind) {
        reject_message(
            state,
            entity.as_deref(),
            op_id,
            kind,
            actor,
            ts,
            format!("invalid msg_kind: {msg_kind}"),
        );
        return;
    }
    if state.messages.contains_key(&msg_id) {
        reject_message(
            state,
            entity.as_deref(),
            op_id,
            kind,
            actor,
            ts,
            format!("duplicate msg_id {msg_id}"),
        );
        return;
    }
    if let Some(key) = idempotency_key.as_deref() {
        if !validate_idempotency_key(key) {
            reject_message(
                state,
                entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                "idempotency_key must be 1..=128 trimmed printable characters".into(),
            );
            return;
        }
        if let Some(existing) = state.message_by_idempotency(actor, key) {
            reject_message(
                state,
                entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!(
                    "idempotency_key `{key}` already used by {}",
                    existing.msg_id
                ),
            );
            return;
        }
    }

    if let Some(e) = entity.as_deref() {
        if !state.beads.contains_key(e) {
            reject(
                state,
                e,
                op_id,
                kind,
                actor,
                ts,
                format!("entity {e} does not exist"),
            );
            return;
        }
    }

    let recipient_status = crate::actor_status::actor_status(
        state,
        &to,
        None,
        ts.parse()
            .expect("message timestamp passed envelope validation"),
        crate::actor_status::DEFAULT_RECENT_WINDOW_S,
    );
    let recipient_presence = crate::state::MsgPresenceEvidence {
        state: recipient_status.presence.state.clone(),
        source: recipient_status.presence.source.clone(),
        reason: recipient_status.presence.reason.clone(),
        as_of_ts: recipient_status.as_of_ts,
    };
    if require_live && recipient_presence.state != "live" {
        reject_message(
            state,
            entity.as_deref(),
            op_id,
            kind,
            actor,
            ts,
            format!(
                "recipient {to} is not live at {}: state={} source={} reason={}",
                recipient_presence.as_of_ts,
                recipient_presence.state,
                recipient_presence.source,
                recipient_presence.reason,
            ),
        );
        return;
    }

    let mut request_state = None;
    let mut reply_transition = None;
    if let Some(parent_id) = reply_to.as_deref() {
        let Some(parent) = state.messages.get(parent_id).cloned() else {
            reject_message(
                state,
                entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("no such reply_to request {parent_id}"),
            );
            return;
        };
        if parent.reply_to.is_some() || parent.msg_kind != "request" {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("msg {parent_id} is not a root request"),
            );
            return;
        }
        if !VALID_REPLY_KINDS.contains(&msg_kind.as_str()) {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                "structured replies must use kind response or decline".into(),
            );
            return;
        }
        if parent.to != actor {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!(
                    "request {parent_id} addressed to {}, not {actor}",
                    parent.to
                ),
            );
            return;
        }
        if to != parent.from {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("reply to {parent_id} must be addressed to {}", parent.from),
            );
            return;
        }
        if parent.request_state != Some(RequestState::Open) {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("request {parent_id} is not open"),
            );
            return;
        }
        if entity.is_some() && entity != parent.entity {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("reply to {parent_id} must use the request entity"),
            );
            return;
        }
        if reservation.is_some() && reservation != parent.reservation {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("reply to {parent_id} must use the request reservation"),
            );
            return;
        }
        let parent_correlation = parent
            .correlation_id
            .clone()
            .unwrap_or_else(|| parent.msg_id.clone());
        if correlation_id
            .as_deref()
            .is_some_and(|value| value != parent_correlation)
        {
            reject_message(
                state,
                parent.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("reply to {parent_id} has a mismatched correlation_id"),
            );
            return;
        }
        entity = parent.entity.clone();
        reservation = parent.reservation.clone();
        correlation_id = Some(parent_correlation);
        let next = if msg_kind == "response" {
            RequestState::Responded
        } else {
            RequestState::Declined
        };
        reply_transition = Some((parent_id.to_string(), next));
    } else {
        if VALID_REPLY_KINDS.contains(&msg_kind.as_str()) {
            reject_message(
                state,
                entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("{msg_kind} requires reply_to"),
            );
            return;
        }
        if msg_kind == "request" {
            correlation_id.get_or_insert_with(|| msg_id.clone());
            request_state = Some(RequestState::Open);
        }
    }

    let bind_entity = entity.clone();
    let response_msg_id = msg_id.clone();
    let answers = match validate_answer_requests(state, actor, &answers, Some(&to)) {
        Ok(answers) => answers,
        Err(reason) => {
            reject_message(state, entity.as_deref(), op_id, kind, actor, ts, reason);
            return;
        }
    };

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
            reply_to,
            correlation_id,
            idempotency_key,
            answers: answers.clone(),
            require_live,
            recipient_presence,
            request_state,
            response_msg_id: None,
            response_post_id: None,
            resolved_op_id: None,
            resolved_ts: None,
            sent_ts: ts.to_string(),
            sent_op_id: op_id.to_string(),
            ack_op_id: None,
            ack_ts: None,
        },
    );

    if let Some((parent_id, next)) = reply_transition {
        let parent = state
            .messages
            .get_mut(&parent_id)
            .expect("validated request disappeared during reducer step");
        parent.request_state = Some(next);
        parent.response_msg_id = Some(response_msg_id.clone());
    }
    for request_id in answers {
        let request = state
            .messages
            .get_mut(&request_id)
            .expect("validated answer request disappeared");
        request.request_state = Some(RequestState::Responded);
        request.response_msg_id = Some(response_msg_id.clone());
    }

    state.push_history(
        bind_entity.as_deref(),
        HistoryEntry::accepted(op_id, kind, actor, ts),
    );
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
            None => {
                state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason))
            }
        }
        return;
    }
    if msg.to != actor {
        let reason = format!("msg {msg_id} addressed to {}, not {}", msg.to, actor);
        match bind_entity.as_deref() {
            Some(e) => reject(state, e, op_id, kind, actor, ts, reason),
            None => {
                state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason))
            }
        }
        return;
    }
    if msg.ack_op_id.is_some() {
        let reason = format!("msg {msg_id} already acked");
        match bind_entity.as_deref() {
            Some(e) => reject(state, e, op_id, kind, actor, ts, reason),
            None => {
                state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason))
            }
        }
        return;
    }

    msg.ack_op_id = Some(op_id.to_string());
    msg.ack_ts = Some(ts.to_string());

    state.push_history(
        bind_entity.as_deref(),
        HistoryEntry::accepted(op_id, kind, actor, ts),
    );
}

fn apply_msg_resolve(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: MsgResolveOp,
) {
    let MsgResolveOp { msg_id, .. } = o;
    let Some(request) = state.messages.get(&msg_id).cloned() else {
        reject_message(
            state,
            None,
            op_id,
            kind,
            actor,
            ts,
            format!("no such request {msg_id}"),
        );
        return;
    };
    if request.reply_to.is_some() || request.msg_kind != "request" {
        reject_message(
            state,
            request.entity.as_deref(),
            op_id,
            kind,
            actor,
            ts,
            format!("msg {msg_id} is not a root request"),
        );
        return;
    }
    if request.from != actor {
        reject_message(
            state,
            request.entity.as_deref(),
            op_id,
            kind,
            actor,
            ts,
            format!("only request sender {} may resolve {msg_id}", request.from),
        );
        return;
    }
    match request.request_state {
        Some(RequestState::Responded | RequestState::Declined) => {}
        Some(RequestState::Open) => {
            reject_message(
                state,
                request.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("request {msg_id} is still open"),
            );
            return;
        }
        Some(RequestState::Resolved) => {
            reject_message(
                state,
                request.entity.as_deref(),
                op_id,
                kind,
                actor,
                ts,
                format!("request {msg_id} already resolved"),
            );
            return;
        }
        None => unreachable!("validated root request without request_state"),
    }

    let request = state
        .messages
        .get_mut(&msg_id)
        .expect("validated request disappeared during reducer step");
    request.request_state = Some(RequestState::Resolved);
    request.resolved_op_id = Some(op_id.to_string());
    request.resolved_ts = Some(ts.to_string());
    let bind_entity = request.entity.clone();
    state.push_history(
        bind_entity.as_deref(),
        HistoryEntry::accepted(op_id, kind, actor, ts),
    );
}

fn reject_orphan(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    reason: String,
) {
    state.push_history(None, HistoryEntry::rejected(op_id, kind, actor, ts, reason));
}

fn apply_board_post(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardPostOp,
) {
    let BoardPostOp {
        post_id,
        topic,
        body,
        reply_to,
        post_kind,
        answers,
        notify,
        idempotency_key,
        ..
    } = o;
    let topic = topic.trim().to_string();
    let post_kind = post_kind.unwrap_or_else(|| "post".to_string());

    if !validate_post_kind(&post_kind) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!(
                "invalid post_kind `{post_kind}` (expected one of: {})",
                VALID_POST_KINDS.join(" | ")
            ),
        );
        return;
    }
    if state.board_posts.contains_key(&post_id) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("duplicate post_id {post_id}"),
        );
        return;
    }
    if let Some(key) = idempotency_key.as_deref() {
        if !validate_idempotency_key(key) {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                "idempotency_key must be 1..=128 trimmed printable characters".into(),
            );
            return;
        }
        if let Some(existing) = state.board_post_by_idempotency(actor, key) {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!(
                    "idempotency_key `{key}` already used by {}",
                    existing.post_id
                ),
            );
            return;
        }
    }
    if topic.trim().is_empty() {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "board post topic must be non-empty".into(),
        );
        return;
    }
    if body.trim().is_empty() {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "board post body must be non-empty".into(),
        );
        return;
    }
    if let Some(parent) = reply_to.as_deref() {
        let Some(parent_post) = state.board_posts.get(parent) else {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("reply_to post {parent} does not exist"),
            );
            return;
        };
        if parent_post.topic != topic {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!(
                    "reply_to post {parent} is in topic {}, not {topic}",
                    parent_post.topic
                ),
            );
            return;
        }
    }

    let answers = match validate_answer_requests(state, actor, &answers, None) {
        Ok(answers) => answers,
        Err(reason) => {
            reject_orphan(state, op_id, kind, actor, ts, reason);
            return;
        }
    };
    let mut explicit_notify = BTreeSet::new();
    for recipient in notify {
        let recipient = recipient.trim();
        if recipient.is_empty()
            || recipient
                .chars()
                .any(|character| matches!(character, '\0' | '\n' | '\r'))
        {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                "notification recipients must be non-empty single-line actor names".into(),
            );
            return;
        }
        if recipient != actor {
            explicit_notify.insert(recipient.to_string());
        }
    }
    let mut notification_recipients: BTreeSet<String> =
        state.topic_watchers(&topic).into_iter().collect();
    notification_recipients.extend(explicit_notify.iter().cloned());
    notification_recipients.remove(actor);

    ensure_topic(state, &topic, actor, ts, op_id);
    if let Some(topic_record) = state.board_topics.get_mut(&topic) {
        topic_record.post_count += 1;
        topic_record.last_activity_ts = ts.to_string();
        topic_record.last_activity_op_id = op_id.to_string();
        match post_kind.as_str() {
            // The newest summary wins: it is a pointer to current state, not a
            // log, so readers never have to pick between two of them.
            "summary" => topic_record.summary_post_id = Some(post_id.clone()),
            "decision" => topic_record.decision_count += 1,
            _ => {}
        }
    }

    let answer_post_id = post_id.clone();
    state
        .board_post_op_index
        .insert(op_id.to_string(), post_id.clone());
    state.board_posts.insert(
        post_id.clone(),
        crate::state::BoardPostRecord {
            post_id,
            from: actor.to_string(),
            topic,
            body,
            reply_to,
            post_kind,
            answers: answers.clone(),
            explicit_notify: explicit_notify.into_iter().collect(),
            notification_recipients: notification_recipients.into_iter().collect(),
            idempotency_key,
            sticky: false,
            sticky_op_id: None,
            superseded_by: None,
            superseded_op_id: None,
            supersedes: Vec::new(),
            retracted: false,
            retraction_reason: None,
            retracted_op_id: None,
            route: crate::state::RouteRecord::default(),
            sent_ts: ts.to_string(),
            sent_op_id: op_id.to_string(),
        },
    );
    for request_id in answers {
        let request = state
            .messages
            .get_mut(&request_id)
            .expect("validated answer request disappeared");
        request.request_state = Some(RequestState::Responded);
        request.response_post_id = Some(answer_post_id.clone());
    }
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_board_watch(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardWatchOp,
) {
    let topic = o.topic.trim();
    if topic.is_empty() || !state.board_topics.contains_key(topic) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("discussion topic {topic} does not exist"),
        );
        return;
    }
    state.board_topic_watches.insert(
        (actor.to_string(), topic.to_string()),
        crate::state::BoardTopicWatchRecord {
            actor: actor.to_string(),
            topic: topic.to_string(),
            watching: o.watching,
            updated_ts: ts.to_string(),
            updated_op_id: op_id.to_string(),
        },
    );
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn validate_answer_requests(
    state: &State,
    actor: &str,
    answers: &[String],
    direct_to: Option<&str>,
) -> Result<Vec<String>, String> {
    let unique: std::collections::BTreeSet<String> = answers.iter().cloned().collect();
    for request_id in &unique {
        let Some(request) = state.messages.get(request_id) else {
            return Err(format!("no such answered request {request_id}"));
        };
        if request.reply_to.is_some() || request.msg_kind != "request" {
            return Err(format!("msg {request_id} is not a root request"));
        }
        if request.to != actor {
            return Err(format!(
                "request {request_id} addressed to {}, not {actor}",
                request.to
            ));
        }
        if request.request_state != Some(RequestState::Open) {
            return Err(format!("request {request_id} is not open"));
        }
        if direct_to.is_some_and(|recipient| recipient != request.from) {
            return Err(format!(
                "message answering {request_id} must be addressed to {}",
                request.from
            ));
        }
    }
    Ok(unique.into_iter().collect())
}

fn apply_board_topic(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardTopicOp,
) {
    let BoardTopicOp {
        topic, title, body, ..
    } = o;
    let topic = topic.trim().to_string();

    if topic.trim().is_empty() {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "discussion topic must be non-empty".into(),
        );
        return;
    }

    match state.board_topics.get_mut(&topic) {
        Some(existing) if existing.explicit => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("discussion topic {topic} already exists"),
            );
            return;
        }
        Some(existing) => {
            existing.explicit = true;
            existing.title = title.unwrap_or_else(|| topic.clone());
            existing.body = body.unwrap_or_default();
            existing.last_activity_ts = ts.to_string();
            existing.last_activity_op_id = op_id.to_string();
        }
        None => {
            state.board_topics.insert(
                topic.clone(),
                crate::state::BoardTopicRecord {
                    topic: topic.clone(),
                    title: title.unwrap_or_else(|| topic.clone()),
                    body: body.unwrap_or_default(),
                    created_by: actor.to_string(),
                    created_ts: ts.to_string(),
                    created_op_id: op_id.to_string(),
                    explicit: true,
                    last_activity_ts: ts.to_string(),
                    last_activity_op_id: op_id.to_string(),
                    post_count: 0,
                    sticky_count: 0,
                    decision_count: 0,
                    summary_post_id: None,
                    route: crate::state::RouteRecord::default(),
                },
            );
        }
    }
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_board_read(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardReadOp,
) {
    let BoardReadOp {
        upto_op_id,
        topic,
        strict,
        ..
    } = o;
    let topic = topic.map(|t| t.trim().to_string());
    if topic.as_deref().is_some_and(str::is_empty) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "board_read topic must be non-empty".into(),
        );
        return;
    }
    let post_topic = match state
        .board_post_op_index
        .get(&upto_op_id)
        .and_then(|post_id| state.board_posts.get(post_id))
        .map(|p| p.topic.clone())
    {
        Some(t) => t,
        None => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("board_read references unknown board_post op {upto_op_id}"),
            );
            return;
        }
    };
    if let Some(topic) = topic.as_deref() {
        if topic != post_topic {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("board_read topic {topic} does not match post topic {post_topic}"),
            );
            return;
        }
    }

    if strict
        && state
            .discussion_cursor_for(actor, topic.as_deref())
            .is_some_and(|cursor| cursor.as_str() > upto_op_id.as_str())
    {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!(
                "board_read boundary {upto_op_id} is older than the effective discussion cursor"
            ),
        );
        return;
    }

    let cursor = if let Some(topic) = topic {
        state
            .board_topic_read_cursors
            .entry((actor.to_string(), topic))
            .or_default()
    } else {
        state
            .board_read_cursors
            .entry(actor.to_string())
            .or_default()
    };
    if cursor.as_str() < upto_op_id.as_str() {
        *cursor = upto_op_id;
    }
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_board_sticky(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardStickyOp,
) {
    let BoardStickyOp {
        post_id, sticky, ..
    } = o;

    let topic = match state.board_posts.get(&post_id) {
        Some(post) => post.topic.clone(),
        None => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("no such board post {post_id}"),
            );
            return;
        }
    };

    let post = state.board_posts.get_mut(&post_id).expect("checked above");
    if post.sticky != sticky {
        post.sticky = sticky;
        post.sticky_op_id = Some(op_id.to_string());
        if let Some(topic_record) = state.board_topics.get_mut(&topic) {
            if sticky {
                topic_record.sticky_count += 1;
            } else {
                topic_record.sticky_count = topic_record.sticky_count.saturating_sub(1);
            }
            topic_record.last_activity_ts = ts.to_string();
            topic_record.last_activity_op_id = op_id.to_string();
        }
    }
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_board_supersede(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardSupersedeOp,
) {
    let BoardSupersedeOp {
        old_post_id,
        new_post_id,
        ..
    } = o;
    if old_post_id == new_post_id {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "a discussion post cannot supersede itself".into(),
        );
        return;
    }
    let Some(old) = state.board_posts.get(&old_post_id).cloned() else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("no such old discussion post {old_post_id}"),
        );
        return;
    };
    let Some(new) = state.board_posts.get(&new_post_id).cloned() else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("no such replacement discussion post {new_post_id}"),
        );
        return;
    };
    let reason = if old.from != actor || new.from != actor {
        Some("only the author of both posts may supersede a discussion post".to_string())
    } else if old.topic != new.topic {
        Some(format!(
            "cannot supersede across topics {} and {}",
            old.topic, new.topic
        ))
    } else if old.disposition() != "active" {
        Some(format!(
            "post {old_post_id} is already {}",
            old.disposition()
        ))
    } else if new.disposition() != "active" {
        Some(format!(
            "replacement post {new_post_id} is {}",
            new.disposition()
        ))
    } else {
        None
    };
    if let Some(reason) = reason {
        reject_orphan(state, op_id, kind, actor, ts, reason);
        return;
    }

    let old = state
        .board_posts
        .get_mut(&old_post_id)
        .expect("validated old post disappeared");
    old.superseded_by = Some(new_post_id.clone());
    old.superseded_op_id = Some(op_id.to_string());
    let new = state
        .board_posts
        .get_mut(&new_post_id)
        .expect("validated replacement post disappeared");
    if !new.supersedes.contains(&old_post_id) {
        new.supersedes.push(old_post_id);
        new.supersedes.sort();
    }
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_board_retract(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardRetractOp,
) {
    let BoardRetractOp {
        post_id, reason, ..
    } = o;
    let reason = reason.trim().to_string();
    let Some(post) = state.board_posts.get(&post_id) else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("no such discussion post {post_id}"),
        );
        return;
    };
    let invalid = if reason.is_empty()
        || reason
            .chars()
            .any(|character| matches!(character, '\0' | '\n' | '\r'))
    {
        Some("discussion retraction reason must be non-empty and single-line".to_string())
    } else if post.from != actor {
        Some(format!("only {} may retract post {post_id}", post.from))
    } else if post.disposition() != "active" {
        Some(format!("post {post_id} is already {}", post.disposition()))
    } else {
        None
    };
    if let Some(reason) = invalid {
        reject_orphan(state, op_id, kind, actor, ts, reason);
        return;
    }
    let post = state
        .board_posts
        .get_mut(&post_id)
        .expect("validated post disappeared");
    post.retracted = true;
    post.retraction_reason = Some(reason);
    post.retracted_op_id = Some(op_id.to_string());
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn ensure_topic(state: &mut State, topic: &str, actor: &str, ts: &str, op_id: &str) {
    state
        .board_topics
        .entry(topic.to_string())
        .or_insert_with(|| crate::state::BoardTopicRecord {
            topic: topic.to_string(),
            title: topic.to_string(),
            body: String::new(),
            created_by: actor.to_string(),
            created_ts: ts.to_string(),
            created_op_id: op_id.to_string(),
            explicit: false,
            last_activity_ts: ts.to_string(),
            last_activity_op_id: op_id.to_string(),
            post_count: 0,
            sticky_count: 0,
            decision_count: 0,
            summary_post_id: None,
            route: crate::state::RouteRecord::default(),
        });
}

fn apply_board_route(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: BoardRouteOp,
) {
    let BoardRouteOp {
        post_id,
        topic,
        route_state,
        entity,
        ..
    } = o;
    let topic = topic.map(|t| t.trim().to_string());

    if !validate_route_state(&route_state) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!(
                "invalid route_state `{route_state}` (expected one of: {})",
                VALID_ROUTE_STATES.join(" | ")
            ),
        );
        return;
    }
    let Some(parsed_state) = RouteState::parse(&route_state) else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("invalid route_state `{route_state}`"),
        );
        return;
    };

    match (post_id.as_deref(), topic.as_deref()) {
        (Some(_), Some(_)) => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                "board_route targets either a post_id or a topic, not both".into(),
            );
            return;
        }
        (None, None) => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                "board_route requires a post_id or a topic".into(),
            );
            return;
        }
        _ => {}
    }

    // `routed` is the only state that carries a bead, and the bead must be a
    // live tracker entity — a link to nothing would defeat the point of routing.
    match (parsed_state, entity.as_deref()) {
        (RouteState::Routed, None) => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                "board_route route_state=routed requires an entity".into(),
            );
            return;
        }
        (state_kind, Some(_)) if state_kind != RouteState::Routed => {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("board_route route_state={route_state} must not carry an entity"),
            );
            return;
        }
        _ => {}
    }
    if let Some(entity) = entity.as_deref() {
        match state.beads.get(entity) {
            Some(bead) if !bead.is_deleted() => {}
            Some(_) => {
                reject_orphan(
                    state,
                    op_id,
                    kind,
                    actor,
                    ts,
                    format!("route target {entity} is deleted"),
                );
                return;
            }
            None => {
                reject_orphan(
                    state,
                    op_id,
                    kind,
                    actor,
                    ts,
                    format!("route target {entity} does not exist"),
                );
                return;
            }
        }
    }

    // Resolve the target, and remember the owning topic so topic activity
    // reflects post-level routing too.
    let (route, activity_topic) = if let Some(post_id) = post_id.as_deref() {
        let Some(post) = state.board_posts.get_mut(post_id) else {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("no such board post {post_id}"),
            );
            return;
        };
        let owning_topic = post.topic.clone();
        (&mut post.route, Some(owning_topic))
    } else {
        let topic_name = topic.as_deref().expect("checked above");
        if topic_name.is_empty() {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                "board_route topic must be non-empty".into(),
            );
            return;
        }
        let Some(topic_record) = state.board_topics.get_mut(topic_name) else {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("no such discussion topic {topic_name}"),
            );
            return;
        };
        (&mut topic_record.route, None)
    };

    route.state = parsed_state;
    // Links accumulate: a discussion can spawn several beads, and re-routing
    // to a second bead must not erase the first.
    if let Some(entity) = entity {
        route.issues.insert(entity);
    }
    route.updated_by = Some(actor.to_string());
    route.updated_ts = Some(ts.to_string());
    route.updated_op_id = Some(op_id.to_string());

    if let Some(topic_name) = activity_topic {
        if let Some(topic_record) = state.board_topics.get_mut(&topic_name) {
            topic_record.last_activity_ts = ts.to_string();
            topic_record.last_activity_op_id = op_id.to_string();
        }
    } else if let Some(topic_name) = topic.as_deref() {
        if let Some(topic_record) = state.board_topics.get_mut(topic_name) {
            topic_record.last_activity_ts = ts.to_string();
            topic_record.last_activity_op_id = op_id.to_string();
        }
    }

    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_session_start(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: SessionStartOp,
) {
    let SessionStartOp {
        session_id,
        ttl_s,
        label,
        pid,
        ..
    } = o;

    if session_id.trim().is_empty() {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "session_id must be non-empty".into(),
        );
        return;
    }
    if ttl_s == 0 {
        reject_orphan(state, op_id, kind, actor, ts, "ttl_s must be > 0".into());
        return;
    }
    let lease_until_ts = match compute_lease_until(ts, ttl_s) {
        Ok(s) => s,
        Err(e) => {
            reject_orphan(state, op_id, kind, actor, ts, format!("bad ttl: {e}"));
            return;
        }
    };

    // A repeated session_start for the same id is a renewal, so a long-running
    // session can extend its lease without minting a new identity.
    let existing_owner = state
        .sessions
        .get(&session_id)
        .map(|s| (s.actor.clone(), s.ended_ts.is_some()));
    if let Some((owner, ended)) = existing_owner {
        if owner != actor {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("session {session_id} belongs to {owner}, not {actor}"),
            );
            return;
        }
        // Ending is terminal. Reviving an ended session would make
        // `session end` a suggestion rather than a fact, and would let a
        // stale lease reappear long after the process behind it is gone.
        if ended {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("session {session_id} has ended; start a new session"),
            );
            return;
        }
        let existing = state.sessions.get_mut(&session_id).expect("checked above");
        existing.ttl_s = ttl_s;
        existing.lease_until_ts = lease_until_ts.clone();
        existing.last_heartbeat_ts = ts.to_string();
        existing.last_heartbeat_op_id = op_id.to_string();
        if label.is_some() {
            existing.label = label;
        }
        if pid.is_some() {
            existing.pid = pid;
        }
        existing
            .heartbeats
            .push(crate::state::SessionHeartbeatRecord {
                ts: ts.to_string(),
                op_id: op_id.to_string(),
                ttl_s,
                lease_until_ts,
                label: existing.label.clone(),
                pid: existing.pid,
            });
        state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
        return;
    }

    state.sessions.insert(
        session_id.clone(),
        crate::state::SessionRecord {
            session_id,
            actor: actor.to_string(),
            label: label.clone(),
            pid,
            started_label: label,
            started_pid: pid,
            ttl_s,
            started_ttl_s: ttl_s,
            started_ts: ts.to_string(),
            started_op_id: op_id.to_string(),
            started_lease_until_ts: lease_until_ts.clone(),
            last_heartbeat_ts: ts.to_string(),
            last_heartbeat_op_id: op_id.to_string(),
            lease_until_ts,
            heartbeats: Vec::new(),
            intent: None,
            intents: Vec::new(),
            ended_ts: None,
            ended_op_id: None,
        },
    );
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_session_heartbeat(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: SessionHeartbeatOp,
) {
    let SessionHeartbeatOp {
        session_id, ttl_s, ..
    } = o;
    if ttl_s == 0 {
        reject_orphan(state, op_id, kind, actor, ts, "ttl_s must be > 0".into());
        return;
    }
    let Some((owner, ended)) = state
        .sessions
        .get(&session_id)
        .map(|session| (session.actor.clone(), session.ended_ts.is_some()))
    else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("no such session {session_id}"),
        );
        return;
    };
    if owner != actor {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("session {session_id} belongs to {owner}, not {actor}"),
        );
        return;
    }
    if ended {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("session {session_id} has ended; start a new session"),
        );
        return;
    }
    let lease_until_ts = match compute_lease_until(ts, ttl_s) {
        Ok(value) => value,
        Err(error) => {
            reject_orphan(state, op_id, kind, actor, ts, format!("bad ttl: {error}"));
            return;
        }
    };
    let session = state.sessions.get_mut(&session_id).expect("checked above");
    session.ttl_s = ttl_s;
    session.last_heartbeat_ts = ts.to_string();
    session.last_heartbeat_op_id = op_id.to_string();
    session.lease_until_ts = lease_until_ts.clone();
    session
        .heartbeats
        .push(crate::state::SessionHeartbeatRecord {
            ts: ts.to_string(),
            op_id: op_id.to_string(),
            ttl_s,
            lease_until_ts,
            label: session.label.clone(),
            pid: session.pid,
        });
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_session_status(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: SessionStatusOp,
) {
    let SessionStatusOp {
        session_id,
        status,
        message,
        issue,
        ..
    } = o;
    if !validate_session_intent(&status) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("invalid session status: {status}"),
        );
        return;
    }
    if message.as_deref().is_some_and(|message| {
        message.is_empty()
            || message.trim() != message
            || message.chars().any(|c| c == '\0' || c == '\n' || c == '\r')
    }) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            "session status message must be non-empty, trimmed, and single-line".into(),
        );
        return;
    }
    let Some(session) = state.sessions.get(&session_id) else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("no such session {session_id}"),
        );
        return;
    };
    if session.actor != actor {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!(
                "session {session_id} belongs to {}, not {actor}",
                session.actor
            ),
        );
        return;
    }
    if session.ended_ts.is_some() {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("session {session_id} has ended; start a new session"),
        );
        return;
    }
    if !session.is_live(ts) {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("session {session_id} is expired; heartbeat it before setting status"),
        );
        return;
    }
    if let Some(issue) = issue.as_deref() {
        let exists = state
            .beads
            .get(issue)
            .is_some_and(|bead| !bead.is_deleted());
        if !exists {
            reject_orphan(
                state,
                op_id,
                kind,
                actor,
                ts,
                format!("no such live issue {issue}"),
            );
            return;
        }
    }
    let session = state.sessions.get_mut(&session_id).expect("checked above");
    let intent = crate::state::SessionIntentRecord {
        state: status,
        message,
        issue,
        set_ts: ts.to_string(),
        set_op_id: op_id.to_string(),
    };
    session.intent = Some(intent.clone());
    session.intents.push(intent);
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
}

fn apply_session_end(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: SessionEndOp,
) {
    let SessionEndOp { session_id, .. } = o;

    let Some(owner) = state.sessions.get(&session_id).map(|s| s.actor.clone()) else {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("no such session {session_id}"),
        );
        return;
    };
    if owner != actor {
        reject_orphan(
            state,
            op_id,
            kind,
            actor,
            ts,
            format!("session {session_id} belongs to {owner}, not {actor}"),
        );
        return;
    }
    let session = state.sessions.get_mut(&session_id).expect("checked above");
    if session.ended_ts.is_none() {
        session.ended_ts = Some(ts.to_string());
        session.ended_op_id = Some(op_id.to_string());
    }
    state.push_history(None, HistoryEntry::accepted(op_id, kind, actor, ts));
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
        v,
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
    let bead_binding = state.beads.get(&entity).filter(|bead| !bead.is_deleted());
    let candidate_binding = state.candidates.get(&entity);
    if bead_binding.is_none() && candidate_binding.is_none() {
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
    if let Some(candidate) = candidate_binding {
        if candidate.phase != crate::candidate::CandidatePhase::Pending {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "candidate-bound reservations require a pending candidate".into(),
            );
            return;
        }
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

    if let Some(candidate) = candidate_binding {
        if let Some(path) = normalized
            .iter()
            .find(|path| !candidate.paths.contains(path))
        {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("candidate reservation path `{path}` is not declared by the candidate"),
            );
            return;
        }
    }

    if let Some(reason) = first_overlap_reason(state, actor, ts, &normalized, v >= 2) {
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
            clock: op_id.to_string(),
            opened_ts: ts.to_string(),
            lease_until_ts,
            closed_paths: std::collections::BTreeSet::new(),
            adoptions: Vec::new(),
        },
    );
    accept(state, &entity, op_id, kind, actor, ts);
}

fn first_overlap_reason(
    state: &State,
    actor: &str,
    ts: &str,
    new_paths: &[String],
    reject_same_actor: bool,
) -> Option<String> {
    for r in state.reservations.values() {
        if !r.is_live(ts) || r.actor == actor && !reject_same_actor {
            continue;
        }
        for p_new in new_paths {
            for p_held in r
                .paths
                .iter()
                .filter(|p| !r.closed_paths.contains(p.as_str()))
            {
                if crate::paths::overlap(p_new, p_held) {
                    if r.actor == actor {
                        return Some(format!(
                            "duplicate reservation: `{p_new}` overlaps `{p_held}` already held by this actor under {} (release or reuse that reservation)",
                            r.reservation_id
                        ));
                    }
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
    r.clock = op_id.to_string();
    accept(state, &entity, op_id, kind, actor, ts);
}

fn apply_reserve_adopt(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: ReserveAdoptOp,
) {
    let ReserveAdoptOp {
        reservation_id,
        entity,
        expect_reservation,
        ttl_s,
        ..
    } = o;
    let Some(reservation) = state.reservations.get(&reservation_id) else {
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
    };
    let old_entity = reservation.entity.clone();
    let old_actor = reservation.actor.clone();
    if reservation.clock != expect_reservation {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            format!("stale reservation CAS: current is {}", reservation.clock),
        );
        return;
    }
    if state.reservation_disposition(reservation, ts) != crate::state::LeaseDisposition::Orphaned {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            "only a still-live orphaned reservation may be adopted".into(),
        );
        return;
    }
    let target_claimed = state.beads.get(&entity).is_some_and(|bead| {
        !bead.is_deleted()
            && bead.status != Status::Closed
            && bead
                .claim
                .as_ref()
                .is_some_and(|claim| claim.claimed_by == actor && claim.is_live(ts))
    });
    if !target_claimed {
        reject(
            state,
            &entity,
            op_id,
            kind,
            actor,
            ts,
            "adopter must hold a live claim on the open target issue".into(),
        );
        return;
    }
    let lease_until_ts = match compute_lease_until(ts, ttl_s) {
        Ok(value) => value,
        Err(error) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                format!("bad ttl: {error}"),
            );
            return;
        }
    };
    let reservation = state
        .reservations
        .get_mut(&reservation_id)
        .expect("reservation checked above");
    reservation
        .adoptions
        .push(crate::state::ReservationAdoption {
            op_id: op_id.to_string(),
            ts: ts.to_string(),
            from_actor: old_actor,
            from_entity: old_entity,
            to_actor: actor.to_string(),
            to_entity: entity.clone(),
        });
    reservation.actor = actor.to_string();
    reservation.entity = entity.clone();
    reservation.ttl_s = ttl_s;
    reservation.lease_until_ts = lease_until_ts;
    reservation.clock = op_id.to_string();
    accept(state, &entity, op_id, kind, actor, ts);
}

fn sorted_unique_nonempty(values: &[String]) -> bool {
    !values.is_empty()
        && values.iter().all(|value| !value.trim().is_empty())
        && values.windows(2).all(|pair| pair[0] < pair[1])
}

fn full_oid(value: &str, object_format: &str) -> bool {
    let expected = if object_format == "sha256" { 64 } else { 40 };
    value.len() == expected
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn apply_candidate_propose(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateProposeOp,
) {
    let candidate_id = o.candidate_id.clone();
    let fail = |state: &mut State, reason: String| {
        reject(state, &candidate_id, op_id, kind, actor, ts, reason)
    };
    if !candidate_id.starts_with("cand-") || candidate_id[5..].parse::<ulid::Ulid>().is_err() {
        fail(state, "candidate id must use the cand-ULID form".into());
        return;
    }
    if state.candidates.contains_key(&candidate_id) {
        fail(state, format!("candidate {candidate_id} already exists"));
        return;
    }
    if state
        .beads
        .get(&o.entity)
        .is_none_or(crate::state::Bead::is_deleted)
    {
        fail(
            state,
            format!("issue {} does not exist or is deleted", o.entity),
        );
        return;
    }
    if o.store_id.trim().is_empty()
        || o.repository_id.trim().is_empty()
        || !matches!(o.object_format.as_str(), "sha1" | "sha256")
    {
        fail(state, "proposal repository identity is incomplete".into());
        return;
    }
    if !full_oid(&o.commit_oid, &o.object_format)
        || !full_oid(&o.base_oid, &o.object_format)
        || !o
            .parent_oids
            .iter()
            .all(|parent| full_oid(parent, &o.object_format))
    {
        fail(
            state,
            "proposal requires full lowercase Git object ids".into(),
        );
        return;
    }
    if actor == o.authorizer || o.reviewers.iter().any(|reviewer| reviewer == actor) {
        fail(
            state,
            "proposer must be distinct from the authorizer and all reviewers".into(),
        );
        return;
    }
    if o.authorizer.trim().is_empty()
        || !sorted_unique_nonempty(&o.reviewers)
        || !sorted_unique_nonempty(&o.paths)
    {
        fail(
            state,
            "authorizer, sorted unique reviewers, and sorted unique paths are required".into(),
        );
        return;
    }
    if o.reviewers.iter().any(|reviewer| reviewer == &o.authorizer) {
        fail(state, "authorizer must be distinct from reviewers".into());
        return;
    }
    if o.evidence_requirements.iter().any(|requirement| {
        requirement.name.trim().is_empty()
            || requirement.kind.trim().is_empty()
            || !sorted_unique_nonempty(&requirement.producers)
    }) {
        fail(
            state,
            "evidence requirements need names, kinds, and sorted unique producers".into(),
        );
        return;
    }
    if !o.evidence_requirements.windows(2).all(|pair| {
        pair[0]
            .name
            .cmp(&pair[1].name)
            .then_with(|| pair[0].kind.cmp(&pair[1].kind))
            .is_lt()
    }) {
        fail(
            state,
            "evidence requirements must be sorted and unique".into(),
        );
        return;
    }
    let ancestry_requirements: Vec<_> = o
        .evidence_requirements
        .iter()
        .filter(|requirement| requirement.name == crate::candidate::GIT_ANCESTRY_EVIDENCE)
        .collect();
    if ancestry_requirements.len() != 1
        || !ancestry_requirements[0]
            .producers
            .iter()
            .any(|producer| producer == actor)
    {
        fail(
            state,
            "exactly one git-ancestry requirement produced by the proposer is mandatory".into(),
        );
        return;
    }

    state.candidates.insert(
        candidate_id.clone(),
        crate::state::CandidateRecord {
            candidate_id: candidate_id.clone(),
            entity: o.entity,
            proposer: actor.to_string(),
            proposal_op_id: op_id.to_string(),
            store_id: o.store_id,
            repository_id: o.repository_id,
            object_format: o.object_format,
            commit_oid: o.commit_oid,
            base_oid: o.base_oid,
            parent_oids: o.parent_oids,
            paths: o.paths,
            authorizer: o.authorizer,
            reviewers: o.reviewers,
            evidence_requirements: o.evidence_requirements,
            evidence_refs: o.evidence_refs,
            phase: crate::candidate::CandidatePhase::Pending,
            phase_op_id: op_id.to_string(),
            successor_id: None,
            reviews: BTreeMap::new(),
            evidence: BTreeMap::new(),
            authorization: None,
            landed: None,
        },
    );
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_evidence(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateEvidenceOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            format!("candidate {candidate_id} does not exist"),
        );
        return;
    };
    if candidate.phase != crate::candidate::CandidatePhase::Pending {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "evidence can only update a pending candidate".into(),
        );
        return;
    }
    let computed = match crate::candidate::evidence_id(&o.payload) {
        Ok(id) => id,
        Err(error) => {
            reject(
                state,
                &candidate_id,
                op_id,
                kind,
                actor,
                ts,
                format!("cannot digest evidence: {error}"),
            );
            return;
        }
    };
    if computed != o.evidence_id {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            format!("evidence id mismatch: expected {computed}"),
        );
        return;
    }
    if o.candidate_oid != candidate.commit_oid {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "evidence candidate object id does not match the immutable proposal".into(),
        );
        return;
    }
    if o.producer_tool.trim().is_empty() {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "evidence producer tool description is required".into(),
        );
        return;
    }
    let required_producer = candidate.evidence_requirements.iter().any(|requirement| {
        requirement.name == o.name
            && requirement.kind == o.evidence_kind
            && requirement.producers.iter().any(|p| p == actor)
    }) || o.name == crate::candidate::GIT_LANDING_EVIDENCE
        && o.evidence_kind == "git"
        && candidate
            .authorization
            .as_ref()
            .is_some_and(|authorization| {
                authorization
                    .grantees
                    .iter()
                    .any(|grantee| grantee == actor)
            });
    if !required_producer {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            format!("{actor} is not a named producer for evidence `{}`", o.name),
        );
        return;
    }
    if o.name == crate::candidate::GIT_ANCESTRY_EVIDENCE {
        match &o.payload {
            crate::candidate::CandidateEvidencePayload::GitAncestry(git)
                if git.repository_id == candidate.repository_id
                    && git.object_format == candidate.object_format
                    && git.commit_oid == candidate.commit_oid
                    && git.base_oid == candidate.base_oid
                    && git.parent_oids == candidate.parent_oids => {}
            _ => {
                reject(
                    state,
                    &candidate_id,
                    op_id,
                    kind,
                    actor,
                    ts,
                    "git-ancestry receipt does not match proposal anchors".into(),
                );
                return;
            }
        }
    }
    let candidate = state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above");
    candidate.evidence.insert(
        (o.name.clone(), actor.to_string()),
        crate::state::CandidateEvidenceRecord {
            producer: actor.to_string(),
            producer_tool: o.producer_tool,
            evidence_id: o.evidence_id,
            name: o.name,
            evidence_kind: o.evidence_kind,
            candidate_oid: o.candidate_oid,
            outcome: o.outcome,
            payload: o.payload,
            refs: o.refs,
            op_id: op_id.to_string(),
            ts: ts.to_string(),
        },
    );
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_review(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateReviewOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "candidate does not exist".into(),
        );
        return;
    };
    if candidate.phase != crate::candidate::CandidatePhase::Pending
        || !candidate.reviewers.iter().any(|reviewer| reviewer == actor)
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "only a named reviewer may review a pending candidate".into(),
        );
        return;
    }
    let current = candidate
        .reviews
        .get(actor)
        .map(|review| review.op_id.as_str());
    if current != o.expect_review.as_deref() {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            format!("stale review CAS: current is {}", current.unwrap_or("none")),
        );
        return;
    }
    if o.evidence_refs.iter().any(|wanted| {
        !candidate
            .evidence
            .values()
            .any(|receipt| &receipt.evidence_id == wanted)
    }) {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "review references unknown evidence".into(),
        );
        return;
    }
    state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above")
        .reviews
        .insert(
            actor.to_string(),
            crate::state::CandidateReviewRecord {
                reviewer: actor.to_string(),
                verdict: o.verdict,
                body: o.body,
                evidence_refs: o.evidence_refs,
                op_id: op_id.to_string(),
                ts: ts.to_string(),
            },
        );
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_authorize(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateAuthorizeOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "candidate does not exist".into(),
        );
        return;
    };
    if candidate.phase != crate::candidate::CandidatePhase::Pending || candidate.authorizer != actor
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "only the named authorizer may authorize a pending candidate".into(),
        );
        return;
    }
    if !matches!(
        o.status,
        crate::candidate::AuthorizationStatus::Granted
            | crate::candidate::AuthorizationStatus::Conditional
    ) || !sorted_unique_nonempty(&o.grantees)
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "authorization requires granted/conditional status and sorted unique grantees".into(),
        );
        return;
    }
    if o.status == crate::candidate::AuthorizationStatus::Granted && !o.conditions.is_empty()
        || o.status == crate::candidate::AuthorizationStatus::Conditional && o.conditions.is_empty()
        || !o.conditions.windows(2).all(|pair| pair[0] < pair[1])
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "granted authorization has no conditions; conditional authorization requires sorted unique conditions".into(),
        );
        return;
    }
    let current = candidate
        .authorization
        .as_ref()
        .map(|authorization| authorization.op_id.as_str());
    if current != o.expect_authorization.as_deref() {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            format!(
                "stale authorization CAS: current is {}",
                current.unwrap_or("none")
            ),
        );
        return;
    }
    state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above")
        .authorization = Some(crate::state::CandidateAuthorizationRecord {
        status: o.status,
        grantees: o.grantees,
        conditions: o.conditions,
        op_id: op_id.to_string(),
        ts: ts.to_string(),
    });
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_revoke(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateRevokeOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "candidate does not exist".into(),
        );
        return;
    };
    let Some(current) = candidate.authorization.as_ref() else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "no authorization to revoke".into(),
        );
        return;
    };
    if candidate.phase != crate::candidate::CandidatePhase::Pending
        || candidate.authorizer != actor
        || current.op_id != o.expect_authorization
        || current.status == crate::candidate::AuthorizationStatus::Consumed
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "revoke requires the named authorizer, pending phase, and current authorization CAS"
                .into(),
        );
        return;
    }
    let authorization = state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above")
        .authorization
        .as_mut()
        .expect("authorization checked above");
    authorization.status = crate::candidate::AuthorizationStatus::Revoked;
    authorization.op_id = op_id.to_string();
    authorization.ts = ts.to_string();
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_supersede(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateSupersedeOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "candidate does not exist".into(),
        );
        return;
    };
    let Some(successor) = state.candidates.get(&o.successor_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "successor does not exist".into(),
        );
        return;
    };
    if candidate_id == o.successor_id
        || candidate.phase != crate::candidate::CandidatePhase::Pending
        || candidate.phase_op_id != o.expect_phase
        || (candidate.proposer != actor && candidate.authorizer != actor)
        || successor.phase != crate::candidate::CandidatePhase::Pending
        || successor.store_id != candidate.store_id
        || successor.repository_id != candidate.repository_id
        || successor.entity != candidate.entity
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "supersede requires proposer ownership, current phase CAS, and a distinct pending same-repository successor".into(),
        );
        return;
    }
    let candidate = state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above");
    candidate.phase = crate::candidate::CandidatePhase::Superseded;
    candidate.phase_op_id = op_id.to_string();
    candidate.successor_id = Some(o.successor_id);
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_abandon(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateAbandonOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "candidate does not exist".into(),
        );
        return;
    };
    if candidate.phase != crate::candidate::CandidatePhase::Pending
        || candidate.phase_op_id != o.expect_phase
        || (candidate.proposer != actor && candidate.authorizer != actor)
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "abandon requires proposer/authorizer ownership and current phase CAS".into(),
        );
        return;
    }
    let candidate = state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above");
    candidate.phase = crate::candidate::CandidatePhase::Abandoned;
    candidate.phase_op_id = op_id.to_string();
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_candidate_landed(
    state: &mut State,
    op_id: &str,
    kind: &str,
    actor: &str,
    ts: &str,
    o: CandidateLandedOp,
) {
    let candidate_id = o.candidate_id.clone();
    let Some(candidate) = state.candidates.get(&candidate_id) else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "candidate does not exist".into(),
        );
        return;
    };
    let authorization = candidate.authorization.as_ref();
    if candidate.phase != crate::candidate::CandidatePhase::Pending
        || candidate.phase_op_id != o.expect_phase
        || authorization.map(|auth| auth.op_id.as_str()) != Some(o.expect_authorization.as_str())
    {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "landed requires current pending phase and authorization CAS".into(),
        );
        return;
    }
    let Some(receipt) = candidate
        .evidence
        .values()
        .find(|receipt| receipt.evidence_id == o.evidence_id)
    else {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "landing evidence not found".into(),
        );
        return;
    };
    let valid_receipt = receipt.outcome == crate::candidate::EvidenceOutcome::Pass
        && receipt.name == crate::candidate::GIT_LANDING_EVIDENCE
        && matches!(
            &receipt.payload,
            crate::candidate::CandidateEvidencePayload::GitLanding(git)
                if git.repository_id == candidate.repository_id
                    && git.candidate_oid == candidate.commit_oid
                    && git.target_ref == o.target_ref
                    && git.authorization_op_id == o.expect_authorization
                    && git.candidate_reachable == Some(true)
        );
    if !valid_receipt {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            "landing receipt is not a passing exact-reachability receipt for this authorization"
                .into(),
        );
        return;
    }
    let landability = state.candidate_landability(&candidate_id, Some(actor));
    if !landability.landable {
        reject(
            state,
            &candidate_id,
            op_id,
            kind,
            actor,
            ts,
            format!(
                "candidate is not landable: {}",
                landability.reason_codes.join(",")
            ),
        );
        return;
    }
    let candidate = state
        .candidates
        .get_mut(&candidate_id)
        .expect("candidate checked above");
    candidate.phase = crate::candidate::CandidatePhase::Landed;
    candidate.phase_op_id = op_id.to_string();
    let authorization = candidate
        .authorization
        .as_mut()
        .expect("authorization checked above");
    authorization.status = crate::candidate::AuthorizationStatus::Consumed;
    candidate.landed = Some(crate::state::CandidateLandedRecord {
        actor: actor.to_string(),
        evidence_id: o.evidence_id,
        authorization_op_id: o.expect_authorization,
        target_ref: o.target_ref,
        op_id: op_id.to_string(),
        ts: ts.to_string(),
    });
    accept(state, &candidate_id, op_id, kind, actor, ts);
}

fn apply_delete(state: &mut State, op_id: &str, kind: &str, actor: &str, ts: &str, o: DeleteOp) {
    let DeleteOp { entity, .. } = o;
    let bead = match state.beads.get_mut(&entity) {
        Some(b) if !b.is_deleted() => b,
        Some(_) => {
            reject(
                state,
                &entity,
                op_id,
                kind,
                actor,
                ts,
                "entity already deleted".into(),
            );
            return;
        }
        None => {
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
        value
            .as_object_mut()
            .unwrap()
            .insert("op".into(), Value::String(String::new()));
        let bytes_for_hash = canonical::encode(&value);
        let name = ids::build_op_name(ts, &bytes_for_hash);
        value
            .as_object_mut()
            .unwrap()
            .insert("op".into(), Value::String(name.as_str().into()));
        let bytes = canonical::encode(&value);
        (format!("{}.json", name.as_str()), bytes)
    }

    fn ts_at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn create_then_patch_disjoint_fields_succeeds() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");

        let create = make_create(
            "alice".into(),
            "bd-1".into(),
            ScalarSet {
                title: Some("hi".into()),
                ..Default::default()
            },
            t1,
        );
        let (n1, b1) = pack(create);

        // Need create_op_id to construct expect for patch.
        let create_op_id = n1.strip_suffix(".json").unwrap().to_string();

        let mut expect = BTreeMap::new();
        expect.insert("priority".into(), create_op_id.clone());
        let patch = make_patch(
            "bob".into(),
            "bd-1".into(),
            expect,
            ScalarSet {
                priority: Some(1),
                ..Default::default()
            },
            t2,
        );
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

        let create = make_create(
            "alice".into(),
            "bd-1".into(),
            ScalarSet {
                title: Some("t".into()),
                ..Default::default()
            },
            t1,
        );
        let (n1, b1) = pack(create);
        let create_op_id = n1.strip_suffix(".json").unwrap().to_string();

        let mut expect = BTreeMap::new();
        expect.insert("status".into(), create_op_id);
        let p1 = make_patch(
            "a".into(),
            "bd-1".into(),
            expect.clone(),
            ScalarSet {
                status: Some(Status::Doing),
                ..Default::default()
            },
            t2,
        );
        let p2 = make_patch(
            "b".into(),
            "bd-1".into(),
            expect,
            ScalarSet {
                status: Some(Status::Blocked),
                ..Default::default()
            },
            t3,
        );
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
        let create = make_create(
            "a".into(),
            "bd-1".into(),
            ScalarSet {
                title: Some("t".into()),
                ..Default::default()
            },
            t1,
        );
        let dep = make_dep(
            true,
            "a".into(),
            "bd-1".into(),
            "bd-1".into(),
            "blocks".into(),
            t2,
        );
        let (n1, b1) = pack(create);
        let (n2, b2) = pack(dep);
        let state = replay([(n1, b1), (n2.clone(), b2)]);
        let id = n2.strip_suffix(".json").unwrap();
        assert!(!state.was_accepted(id));
        let reason = state.rejection_reason(id).unwrap_or_default();
        assert!(
            reason.contains("self"),
            "expected reason about self-dep, got: {reason:?}"
        );
    }

    #[test]
    fn tag_add_idempotent() {
        let t1 = ts_at("2026-04-20T18:24:55.000001Z");
        let t2 = ts_at("2026-04-20T18:24:55.000002Z");
        let t3 = ts_at("2026-04-20T18:24:55.000003Z");
        let create = make_create(
            "a".into(),
            "bd-1".into(),
            ScalarSet {
                title: Some("t".into()),
                ..Default::default()
            },
            t1,
        );
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
        let create = make_create(
            "a".into(),
            "bd-1".into(),
            ScalarSet {
                title: Some("t".into()),
                ..Default::default()
            },
            t1,
        );
        let (n1, b1) = pack(create);
        let create_op_id = n1.strip_suffix(".json").unwrap().to_string();
        let mut expect = BTreeMap::new();
        expect.insert("title".into(), create_op_id);
        let patch = make_patch(
            "a".into(),
            "bd-1".into(),
            expect,
            ScalarSet {
                title: Some("t2".into()),
                ..Default::default()
            },
            t2,
        );
        let (n2, b2) = pack(patch);

        let s1 = replay([(n1.clone(), b1.clone()), (n2.clone(), b2.clone())]);
        let s2 = replay([(n1, b1), (n2, b2)]);
        assert_eq!(s1.beads["bd-1"].title, s2.beads["bd-1"].title);
        assert_eq!(s1.beads["bd-1"].clock, s2.beads["bd-1"].clock);
    }
}
