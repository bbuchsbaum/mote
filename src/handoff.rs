//! Atomic holder-checked handoff. A note, transfer, and optional release share
//! one reducer decision; a reservation is never implicitly adopted or renewed.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{MoteError, MoteResult, Store, authority, candidate, ids, op, reducer, state};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffOp {
    pub v: u32,
    pub op: String,
    pub ts: String,
    pub actor: String,
    pub entity: String,
    pub to: String,
    pub expect_holder: String,
    pub expect_claim: String,
    pub ttl_s: u32,
    pub note: String,
    pub release: bool,
    pub idempotency_key: String,
}

pub(crate) fn apply(state: &mut state::State, id: &str, o: HandoffOp) {
    let reject = |state: &mut state::State, reason: String| {
        state.push_history(
            Some(&o.entity),
            state::HistoryEntry::rejected(id, "handoff", &o.actor, &o.ts, reason),
        );
    };
    let digest = match candidate::action_digest(&o) {
        Ok(digest) => digest,
        Err(error) => {
            reject(state, error.to_string());
            return;
        }
    };
    if let Some((previous_digest, previous_op)) = state
        .handoff_idempotency
        .get(&(o.actor.clone(), o.idempotency_key.clone()))
    {
        reject(
            state,
            if *previous_digest == digest {
                format!("idempotent retry: original handoff {previous_op}")
            } else {
                format!("idempotency conflict: key used by {previous_op}")
            },
        );
        return;
    }
    if !op::validate_idempotency_key(&o.idempotency_key) {
        reject(state, "invalid idempotency key".into());
        return;
    }
    state.handoff_idempotency.insert(
        (o.actor.clone(), o.idempotency_key.clone()),
        (digest, id.into()),
    );
    let reason = match state.beads.get(&o.entity) {
        None => Some("conflict: issue does not exist"),
        Some(b) if b.is_deleted() || b.status == op::Status::Closed => {
            Some("conflict: issue is closed or deleted")
        }
        Some(b) => match &b.claim {
            None => Some("conflict: no claim to hand off"),
            Some(c) if c.claimed_by != o.expect_holder || c.claim_clock != o.expect_claim => {
                Some("conflict: holder or claim token changed")
            }
            Some(c) if c.claimed_by != o.actor => {
                Some("unauthorized: sender is not the current holder")
            }
            Some(c) if !c.is_live(&o.ts) => Some("expired: claim lease elapsed"),
            Some(_) => None,
        },
    };
    if let Some(reason) = reason {
        reject(state, reason.into());
        return;
    }
    if o.to.trim().is_empty() || o.to.trim() != o.to || o.ttl_s == 0 {
        reject(state, "invalid recipient or zero claim TTL".into());
        return;
    }
    let until = o.ts.parse::<Timestamp>().ok().and_then(|ts| {
        ts.checked_add(jiff::SignedDuration::from_secs(i64::from(o.ttl_s)))
            .ok()
    });
    let Some(until) = until else {
        reject(state, "invalid claim TTL".into());
        return;
    };
    let bead = state.beads.get_mut(&o.entity).expect("checked issue");
    bead.claim = Some(state::ClaimState {
        claimed_by: o.to,
        claim_clock: id.into(),
        lease_until_ts: ids::format_rfc3339(until),
        session: None,
    });
    bead.released_claim = None;
    bead.notes.push(state::Note {
        op_id: id.into(),
        note_kind: "handoff".into(),
        actor: o.actor.clone(),
        ts: o.ts.clone(),
        text: o.note,
    });
    for reservation in state
        .reservations
        .values_mut()
        .filter(|r| r.actor == o.actor && r.entity == o.entity)
    {
        if o.release {
            reservation
                .closed_paths
                .extend(reservation.paths.iter().cloned());
            reservation.clock = id.into();
        } else {
            state
                .handoff_orphans
                .insert(reservation.reservation_id.clone());
        }
    }
    state.push_history(
        Some(&o.entity),
        state::HistoryEntry::accepted(id, "handoff", &o.actor, &o.ts),
    );
}

/// Execute a strict handoff. Supplied holder/token are never replaced. Legacy
/// interactive callers may omit both, in which case only their own live claim
/// is selected under the same boundary that commits the operation.
pub fn execute(
    store: &Store,
    mut request: HandoffOp,
    explicit_preconditions: bool,
) -> MoteResult<(i32, Value)> {
    if !op::validate_idempotency_key(&request.idempotency_key) {
        return Err(MoteError::Invalid("invalid idempotency key".into()));
    }
    let writer = authority::Writer::acquire(store)?;
    writer.ensure_no_landing()?;
    writer.enable()?;
    let state = reducer::replay_store(store)?;
    if let Some((_, original)) = state
        .handoff_idempotency
        .get(&(request.actor.clone(), request.idempotency_key.clone()))
    {
        let bytes = std::fs::read(store.ops_dir().join(format!("{original}.json")))?;
        let op::Op::Handoff(previous) = serde_json::from_slice(&bytes)? else {
            return Err(MoteError::Other("invalid handoff retry record".into()));
        };
        if !explicit_preconditions {
            request.expect_holder = previous.expect_holder.clone();
            request.expect_claim = previous.expect_claim.clone();
        }
        if candidate::action_digest(&request)? != candidate::action_digest(&previous)? {
            return Err(MoteError::Rejected(
                "idempotency conflict: handoff arguments changed".into(),
            ));
        }
        return Ok(outcome(&state, &previous, original, true));
    }
    if !explicit_preconditions {
        request.expect_holder = request.actor.clone();
        request.expect_claim = state
            .beads
            .get(&request.entity)
            .and_then(|b| b.claim.as_ref())
            .filter(|c| c.claimed_by == request.actor)
            .map(|c| c.claim_clock.clone())
            .unwrap_or_default();
    }
    request.ts = ids::format_rfc3339(Timestamp::now());
    let prepared = authority::PreparedOp::new(&op::Op::Handoff(request.clone()))?;
    if let Err(error) = writer.publish(&prepared) {
        return Ok((
            2,
            json!({"outcome": "recovery_required", "accepted": null,
            "op_id": prepared.name, "idempotency_key": request.idempotency_key,
            "journal": authority::directory(store).join("publication.json"),
            "detail": error.to_string(), "reservations_transferred": false}),
        ));
    }
    let state = reducer::replay_store(store)?;
    Ok(outcome(&state, &request, &prepared.name, false))
}

fn outcome(state: &state::State, request: &HandoffOp, id: &str, retry: bool) -> (i32, Value) {
    let accepted = state.was_accepted(id);
    let reason = state.rejection_reason(id);
    let now = ids::format_rfc3339(Timestamp::now());
    let claim = state
        .beads
        .get(&request.entity)
        .and_then(|b| b.claim.as_ref());
    let current = claim.is_some_and(|c| c.claim_clock == id && c.is_live(&now));
    let disposition = if accepted {
        if current {
            "transferred"
        } else {
            "applied_no_longer_current"
        }
    } else if reason.as_deref().is_some_and(|s| s.starts_with("expired:")) {
        "expired"
    } else if reason
        .as_deref()
        .is_some_and(|s| s.starts_with("unauthorized:"))
    {
        "unauthorized"
    } else {
        "conflict"
    };
    (
        if accepted { 0 } else { 2 },
        json!({
            "outcome": disposition, "accepted": accepted, "retry": retry,
            "op_id": id, "idempotency_key": request.idempotency_key,
            "reason": reason, "claim_current": current,
            "current_claim": claim.map(|c| json!({"holder": c.claimed_by, "token": c.claim_clock, "lease_until_ts": c.lease_until_ts})),
            "reservations_transferred": false,
            "reservations": state.reservations.values().filter(|r| r.entity == request.entity).map(|r| json!({
                "reservation_id": r.reservation_id, "holder": r.actor, "token": r.clock,
                "lease_until_ts": r.lease_until_ts, "disposition": state.reservation_disposition(r, &now),
            })).collect::<Vec<_>>(),
        }),
    )
}
