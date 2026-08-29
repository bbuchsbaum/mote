//! `mote watch`: stream snapshots of derived state whenever the op log changes.
//!
//! Read-only by construction: the implementation only ever calls
//! `reducer::replay_store`. Writes are produced by the regular CLI; this
//! command is a passive observer.

use jiff::Timestamp;
use serde_json::Value;

use crate::errors::MoteResult;
use crate::events::StoreWatcher;
use crate::ids;
use crate::reducer;
use crate::repo::Store;
use crate::state::{LeaseDisposition, State};

/// Run the watch loop. Returns only on a fatal error; SIGINT/Ctrl-C is the
/// normal way to exit.
pub fn run(
    store: &Store,
    actor: Option<&str>,
    json_mode: bool,
    interval_s: u64,
) -> MoteResult<i32> {
    let actor = actor.map(String::from);

    // Subscribe before the first snapshot. If an op lands while that snapshot
    // is replaying, the queued notification causes a second replay rather than
    // leaving the display stale until the fallback tick.
    let watcher = StoreWatcher::new(store, interval_s)?;

    // A fresh viewer always sees the current state, not just future changes.
    emit_snapshot(store, actor.as_deref(), json_mode)?;

    while watcher.wait() {
        emit_snapshot(store, actor.as_deref(), json_mode)?;
    }

    Ok(0)
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
pub fn snapshot_value(state: &State, actor: Option<&str>, now: &str) -> Value {
    use std::collections::BTreeMap;
    let parsed_as_of = now.parse::<Timestamp>().ok();
    let snapshot_ts = parsed_as_of
        .map(ids::format_rfc3339)
        .unwrap_or_else(|| now.to_string());
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for b in state.live_beads() {
        *counts.entry(b.status.as_str()).or_insert(0) += 1;
    }
    let active_claims: Vec<Value> = state
        .live_beads()
        .filter(|b| state.claim_disposition(b, now) == LeaseDisposition::Active)
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
        .filter(|r| state.reservation_disposition(r, now) == LeaseDisposition::Active)
        .map(|r| {
            serde_json::json!({
                "reservation_id": r.reservation_id,
                "actor": r.actor,
                "entity": r.entity,
                "binding_kind": state.reservation_binding_kind(r),
                "paths": r.live_paths(),
                "lease_until_ts": r.lease_until_ts,
            })
        })
        .collect();
    let orphaned_claims: Vec<Value> = state
        .beads
        .values()
        .filter(|b| state.claim_disposition(b, now) == LeaseDisposition::Orphaned)
        .map(|b| {
            serde_json::json!({
                "id": b.id,
                "title": b.title,
                "claimed_by": b.claim.as_ref().map(|c| &c.claimed_by),
                "lease_until_ts": b.claim.as_ref().map(|c| &c.lease_until_ts),
                "disposition": "orphaned",
            })
        })
        .collect();
    let orphaned_reservations: Vec<Value> = state
        .reservations
        .values()
        .filter(|r| state.reservation_disposition(r, now) == LeaseDisposition::Orphaned)
        .map(|r| {
            serde_json::json!({
                "reservation_id": r.reservation_id,
                "actor": r.actor,
                "entity": r.entity,
                "binding_kind": state.reservation_binding_kind(r),
                "paths": r.live_paths(),
                "lease_until_ts": r.lease_until_ts,
                "clock": r.clock,
                "disposition": "orphaned",
                "adoptions": r.adoptions,
            })
        })
        .collect();
    let expiring_reservations: Vec<Value> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, now)
                == Some(crate::state::ReservationExpiryPhase::Expiring)
        })
        .map(|reservation| {
            serde_json::json!({
                "reservation_id": reservation.reservation_id,
                "holder": reservation.actor,
                "entity": reservation.entity,
                "binding_kind": state.reservation_binding_kind(reservation),
                "paths": reservation.live_paths(),
                "warning_at": state.reservation_warning_ts(reservation),
                "deadline": reservation.lease_until_ts,
                "reason": "ttl_near_deadline",
            })
        })
        .collect();
    let expired_reservations: Vec<Value> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, now)
                == Some(crate::state::ReservationExpiryPhase::Expired)
        })
        .map(|reservation| {
            serde_json::json!({
                "reservation_id": reservation.reservation_id,
                "holder": reservation.actor,
                "entity": reservation.entity,
                "binding_kind": state.reservation_binding_kind(reservation),
                "paths": reservation.live_paths(),
                "deadline": reservation.lease_until_ts,
                "reason": "ttl_elapsed",
            })
        })
        .collect();
    let inbox_unacked = actor.map(|a| state.inbox_for(a).len()).unwrap_or(0);
    let discussion_unread = actor
        .map(|a| state.unread_board_posts_for(a, None).len())
        .unwrap_or(0);
    let candidates: Vec<Value> = state
        .candidates
        .values()
        .map(|candidate| {
            let landability = state.candidate_landability(&candidate.candidate_id, actor);
            serde_json::json!({
                "candidate_id": candidate.candidate_id,
                "entity": candidate.entity,
                "proposer": candidate.proposer,
                "phase": candidate.phase,
                "phase_op_id": candidate.phase_op_id,
                "commit_oid": candidate.commit_oid,
                "policy": {
                    "paths": candidate.paths,
                    "authorizer": candidate.authorizer,
                    "reviewers": candidate.reviewers,
                    "evidence_requirements": candidate.evidence_requirements,
                },
                "reviews": candidate.reviews,
                "evidence": candidate.evidence.values().collect::<Vec<_>>(),
                "authorization": candidate.authorization,
                "successor_id": candidate.successor_id,
                "reservations": state.candidate_reservations(&candidate.candidate_id).iter().map(|reservation| serde_json::json!({
                    "reservation_id": reservation.reservation_id,
                    "actor": reservation.actor,
                    "paths": reservation.live_paths(),
                    "lease_until_ts": reservation.lease_until_ts,
                    "disposition": state.reservation_disposition(reservation, now),
                })).collect::<Vec<_>>(),
                "landability": landability,
            })
        })
        .collect();
    let actors = parsed_as_of
        .map(|as_of| {
            crate::actor_status::actor_statuses(
                state,
                actor,
                as_of,
                crate::actor_status::DEFAULT_RECENT_WINDOW_S,
            )
        })
        .unwrap_or_default();

    serde_json::json!({
        "ts": snapshot_ts,
        "actor": actor,
        "status_counts": counts,
        "active_claims": active_claims,
        "active_reservations": active_reservations,
        "orphaned_claims": orphaned_claims,
        "orphaned_reservations": orphaned_reservations,
        "expiring_reservations": expiring_reservations,
        "expired_reservations": expired_reservations,
        "inbox_unacked": inbox_unacked,
        "discussion_unread": discussion_unread,
        "board_topics": state.board_topics.len(),
        "board_posts": state.board_posts.len(),
        "candidates": candidates,
        "actors": actors,
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
    if let Ok(as_of) = now.parse::<Timestamp>() {
        let actors = crate::actor_status::actor_statuses(
            state,
            actor,
            as_of,
            crate::actor_status::DEFAULT_RECENT_WINDOW_S,
        );
        println!("actors:       {} known", actors.len());
        for status in actors.iter().take(10) {
            println!(
                "  {} {} source={} reason={} as-of={} sessions={} inbox={} requests={}",
                status.actor,
                status.presence.state,
                status.presence.source,
                status.presence.reason,
                status.as_of_ts,
                status.presence.live_session_count,
                status.attention.inbox_unacked,
                status.attention.incoming_open_requests,
            );
        }
        if actors.len() > 10 {
            println!("  … {} more", actors.len() - 10);
        }
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
        .filter(|b| state.claim_disposition(b, now) == LeaseDisposition::Active)
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
        .filter(|r| state.reservation_disposition(r, now) == LeaseDisposition::Active)
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

    let orphaned_claims: Vec<_> = state
        .beads
        .values()
        .filter(|b| state.claim_disposition(b, now) == LeaseDisposition::Orphaned)
        .collect();
    let orphaned_reservations: Vec<_> = state
        .reservations
        .values()
        .filter(|r| state.reservation_disposition(r, now) == LeaseDisposition::Orphaned)
        .collect();
    println!(
        "orphans:      {} claims, {} reservations",
        orphaned_claims.len(),
        orphaned_reservations.len()
    );
    for r in orphaned_reservations.iter().take(10) {
        println!(
            "  ORPHAN {} by {} on {}: {}",
            r.reservation_id,
            r.actor,
            r.entity,
            r.live_paths().join(", ")
        );
    }

    let expiring: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, now)
                == Some(crate::state::ReservationExpiryPhase::Expiring)
        })
        .collect();
    let expired: Vec<_> = state
        .reservations
        .values()
        .filter(|reservation| {
            state.reservation_expiry_phase(reservation, now)
                == Some(crate::state::ReservationExpiryPhase::Expired)
        })
        .collect();
    println!(
        "expiry:       {} warning, {} expired",
        expiring.len(),
        expired.len()
    );
    for reservation in expiring.iter().take(10) {
        println!(
            "  EXPIRING {} by {} deadline {}: {}",
            reservation.reservation_id,
            reservation.actor,
            reservation.lease_until_ts,
            reservation.live_paths().join(", ")
        );
    }
    for reservation in expired.iter().take(10) {
        println!(
            "  EXPIRED {} by {} deadline {} reason=ttl_elapsed: {}",
            reservation.reservation_id,
            reservation.actor,
            reservation.lease_until_ts,
            reservation.live_paths().join(", ")
        );
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

    println!("candidates:   {} total", state.candidates.len());
    for candidate in state.candidates.values().take(10) {
        let landability = state.candidate_landability(&candidate.candidate_id, actor);
        let disposition = if landability.landable {
            "landable".to_string()
        } else {
            landability.reason_codes.join(",")
        };
        println!(
            "  {} {} issue={} {}",
            candidate.candidate_id,
            candidate.phase.as_str(),
            candidate.entity,
            disposition
        );
    }
    if state.candidates.len() > 10 {
        println!("  … {} more", state.candidates.len() - 10);
    }

    let total_ops: usize =
        state.history.values().map(Vec::len).sum::<usize>() + state.orphan_history.len();
    println!("ops total:    {total_ops}");
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::candidate::{CandidatePhase, EvidenceRequirement, GIT_ANCESTRY_EVIDENCE};
    use crate::op::make_session_start;
    use crate::state::CandidateRecord;
    use crate::{publish, reducer, repo::Store};
    use tempfile::TempDir;

    #[test]
    fn periodic_snapshot_time_refreshes_presence_after_lease_expiry() {
        let temp = TempDir::new().unwrap();
        let store = Store::init(temp.path()).unwrap();
        let started: Timestamp = "2030-01-01T00:00:00Z".parse().unwrap();
        let op = make_session_start("alice".into(), "sess-a".into(), 60, None, None, started);
        publish::publish_op(&store, &op).unwrap();
        let state = reducer::replay_store(&store).unwrap();

        let live = snapshot_value(&state, Some("alice"), "2030-01-01T00:00:30Z");
        let expired = snapshot_value(&state, Some("alice"), "2030-01-01T00:01:01Z");
        assert_eq!(live["actors"][0]["presence"]["state"], "live");
        assert_eq!(expired["actors"][0]["presence"]["state"], "expired");
        assert_eq!(live["actors"][0]["as_of_ts"], live["ts"]);
        assert_eq!(expired["actors"][0]["as_of_ts"], expired["ts"]);
    }

    #[test]
    fn snapshot_candidate_schema_retains_structured_landability_reasons() {
        let candidate_id = "cand-01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string();
        let candidate = CandidateRecord {
            candidate_id: candidate_id.clone(),
            entity: "bd-test".into(),
            proposer: "proposer".into(),
            proposal_op_id: "op-proposal".into(),
            store_id: "st-test".into(),
            repository_id: "repo-test".into(),
            object_format: "sha1".into(),
            commit_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            base_oid: "1111111111111111111111111111111111111111".into(),
            parent_oids: vec!["1111111111111111111111111111111111111111".into()],
            paths: vec!["src/lib.rs".into()],
            authorizer: "authorizer".into(),
            reviewers: vec!["reviewer".into()],
            evidence_requirements: vec![EvidenceRequirement {
                name: GIT_ANCESTRY_EVIDENCE.into(),
                kind: "git".into(),
                producers: vec!["proposer".into()],
            }],
            evidence_refs: Vec::new(),
            phase: CandidatePhase::Pending,
            phase_op_id: "op-proposal".into(),
            successor_id: None,
            reviews: BTreeMap::new(),
            evidence: BTreeMap::new(),
            authorization: None,
            landed: None,
        };
        let mut state = State::default();
        state.candidates.insert(candidate_id.clone(), candidate);
        let value = snapshot_value(&state, Some("lander"), "2026-08-29T00:00:00Z");
        assert_eq!(value["candidates"][0]["candidate_id"], candidate_id);
        assert_eq!(value["candidates"][0]["phase"], "pending");
        assert!(
            value["candidates"][0]["landability"]["reason_codes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "git_evidence_missing")
        );
    }
}
