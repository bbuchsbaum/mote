//! Fresh, read-only projections. Remote ownership never uses legacy TTLs.
use std::collections::BTreeSet;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};

use super::{open_client, print_value, view_with_data};
use crate::cli::{ActorCmd, Cli, Command};
use crate::events::{EventFilter, event_from_op};
use crate::git_authority::{compatibility, coordination::Transition, history::Snapshot, protocol};
use crate::{MoteError, MoteResult, Store, op::Op};

pub(super) fn run(cli: &Cli, store: &Store) -> MoteResult<i32> {
    // Reject unsupported modes before network access or any local maintenance.
    match &cli.command {
        Command::Fsck { clean_tmp: true } => {
            return invalid("remote fsck is read-only; --clean-tmp is unsupported");
        }
        Command::Audit {
            target_ref,
            stale_after,
            fail_on,
        } => {
            if target_ref.is_some() || stale_after.is_some() {
                return invalid(
                    "remote audit validates the canonical chain only; candidate reachability and age-based lease findings are unsupported",
                );
            }
            if !matches!(fail_on.as_str(), "error" | "warning" | "never") {
                return invalid("invalid --fail-on (expected error | warning | never)");
            }
        }
        Command::Actor {
            cmd:
                ActorCmd::List {
                    presence,
                    active_within,
                },
        } => {
            if presence.is_some() || active_within.is_some() {
                return invalid(
                    "remote actor list has explicit session enrollment, not TTL presence; --presence and --active-within are unsupported",
                );
            }
        }
        Command::Events { interval: 0, .. } | Command::Watch { interval: 0 } => {
            return invalid("poll interval must be positive");
        }
        _ => {}
    }
    let (client, _scratch) = open_client(store)?;
    match &cli.command {
        Command::Events {
            kinds,
            for_actor,
            after,
            follow,
            interval,
        } => {
            let filter = EventFilter::new(kinds, for_actor.clone())?;
            let mut cursor = after.clone();
            loop {
                let snapshot = client.refresh()?;
                let events = positioned_events(&snapshot, &filter)?;
                let start = match &cursor {
                    Some(cursor) => events.iter().position(|event| event.cursor == *cursor)
                        .ok_or_else(|| MoteError::Invalid("event cursor is absent from this authority root; use a cursor emitted by this authority".into()))? + 1,
                    None => 0,
                };
                for event in &events[start..] {
                    if event.matches {
                        print_value(
                            cli.json,
                            view_with_data(
                                &snapshot,
                                json!({"cursor":event.cursor,"phase":event.phase,"position":event.position,"event":event.event}),
                            ),
                        );
                    }
                }
                if let Some(last) = events.last() {
                    cursor = Some(last.cursor.clone());
                }
                if !follow {
                    return Ok(0);
                }
                std::thread::sleep(Duration::from_secs(*interval));
            }
        }
        Command::Watch { interval } => {
            let mut head = None;
            loop {
                let snapshot = client.refresh()?;
                if head.as_ref() != Some(&snapshot.head) {
                    print_value(
                        cli.json,
                        view_with_data(&snapshot, snapshot.state.coordination.view()?),
                    );
                    head = Some(snapshot.head);
                }
                std::thread::sleep(Duration::from_secs(*interval));
            }
        }
        Command::Actor { cmd } => {
            let snapshot = client.refresh()?;
            let domain = snapshot.state.coordination.communication_state()?;
            let view = snapshot.state.coordination.view()?;
            let data = match cmd {
                ActorCmd::List { .. } => {
                    let mut names = crate::actor_status::known_actor_names(&domain);
                    names.extend(
                        view["sessions"]
                            .as_object()
                            .into_iter()
                            .flatten()
                            .filter_map(|(_, s)| s["actor"].as_str().map(str::to_owned)),
                    );
                    json!(
                        names
                            .iter()
                            .map(|actor| actor_value(&snapshot, &domain, &view, actor, 600))
                            .collect::<MoteResult<Vec<_>>>()?
                    )
                }
                ActorCmd::Status {
                    name,
                    recent_window,
                } => {
                    let actor = match name {
                        Some(name) => name.clone(),
                        None => store.resolve_actor(cli.actor.as_deref())?,
                    };
                    actor_value(&snapshot, &domain, &view, &actor, *recent_window)?
                }
                _ => unreachable!("remote actor projection"),
            };
            print_value(cli.json, view_with_data(&snapshot, data));
            Ok(0)
        }
        Command::Doctor | Command::Fsck { .. } | Command::Audit { .. } => {
            let snapshot = client.refresh()?;
            let command = match cli.command {
                Command::Doctor => "doctor",
                Command::Fsck { .. } => "fsck",
                _ => "audit",
            };
            print_value(
                cli.json,
                view_with_data(
                    &snapshot,
                    json!({
                        "command":command,"scope":"git-authority-canonical-chain-v1",
                        "canonical_chain_verified":true,"manifest":snapshot.manifest,
                        "binary":compatibility::binary_evidence()?,
                        "compatibility":compatibility::compare_manifest(&snapshot.manifest),
                        "legacy_archive_verified":snapshot.manifest.legacy_digest.is_some(),
                        "local_actor":store.resolve_actor(cli.actor.as_deref()).ok(),
                        "findings":[],"excluded":["local-op-log","candidate-reachability","TTL-presence","local-cleanup"]
                    }),
                ),
            );
            Ok(0)
        }
        _ => unreachable!("remote observation routing"),
    }
}

fn actor_value(
    snapshot: &Snapshot,
    state: &crate::state::State,
    view: &Value,
    actor: &str,
    recent_window: u32,
) -> MoteResult<Value> {
    let sessions: Vec<_> = view["sessions"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, s)| s["actor"].as_str() == Some(actor))
        .map(|(id, s)| json!({"session_id":id,"session":s}))
        .collect();
    let enrolled = sessions.iter().any(|s| s["session"]["status"] == "active");
    let claims: Vec<_> = view["claims"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, c)| c["actor"].as_str() == Some(actor))
        .map(|(id, c)| json!({"task":id,"claim":c}))
        .collect();
    let reservations: Vec<_> = view["reservations"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, r)| r["actor"].as_str() == Some(actor) && r["status"] == "open")
        .map(|(id, r)| json!({"reservation_id":id,"reservation":r}))
        .collect();
    let now = Timestamp::now().as_second();
    let mut recent = Vec::new();
    let mut latest = None;
    for (path, bytes) in &snapshot.files {
        if !path.starts_with("transactions/") {
            continue;
        }
        let tx: protocol::Transaction = protocol::decode(bytes)?;
        if tx.request.actor == actor && tx.outcome.status == protocol::OutcomeStatus::Accepted {
            let evidence = json!({"sequence":tx.sequence,"request_id":tx.request.request_id,"timestamp":tx.request.timestamp});
            let seconds = tx
                .request
                .timestamp
                .parse::<Timestamp>()
                .map_err(|e| MoteError::Invalid(e.to_string()))?
                .as_second();
            if seconds <= now && now.saturating_sub(seconds) <= i64::from(recent_window) {
                recent.push(evidence.clone());
            }
            latest = Some(evidence);
        }
    }
    Ok(
        json!({"actor":actor,"enrollment":if enrolled {"active"} else if sessions.is_empty() {"untracked"} else {"inactive"},
        "process_liveness":"unknown","sessions":sessions,"claims":claims,"reservations":reservations,
        "attention":{"unacked_messages":state.inbox_for(actor).iter().map(|m| &m.msg_id).collect::<Vec<_>>(),
            "open_requests":state.requests_for(actor).iter().filter(|m| m.request_state == Some(crate::state::RequestState::Open)).map(|m| &m.msg_id).collect::<Vec<_>>(),
            "unread_notifications":state.unread_board_notifications_for(actor,None).iter().map(|p| &p.post_id).collect::<Vec<_>>()},
        "activity":{"latest_accepted_request":latest,"recent_window_seconds":recent_window,"recent_accepted_requests":recent,"timestamp_source":"client-supplied; descriptive only"}}),
    )
}

struct PositionedEvent {
    cursor: String,
    phase: &'static str,
    position: Value,
    event: Value,
    matches: bool,
}

fn positioned_events(
    snapshot: &Snapshot,
    filter: &EventFilter,
) -> MoteResult<Vec<PositionedEvent>> {
    let domain = snapshot.state.coordination.communication_state()?;
    let mut out = Vec::new();
    if let Some(pack) = snapshot.files.get("legacy.pack") {
        let files = crate::git_authority::legacy::unpack(pack)?;
        let export: crate::git_authority::legacy::Export =
            protocol::decode(&files["legacy/export.json"])?;
        for entry in export.source.entries {
            let id = entry.name.strip_suffix(".json").unwrap_or(&entry.name);
            if !domain.was_accepted(id) {
                continue;
            }
            // Legacy replay uses serde's original decoder, including its
            // historical handling of duplicate JSON fields.
            let op: Op = serde_json::from_slice(&files[&entry.path])?;
            let matches = filter.matches(&op, &domain);
            let event = event_from_op(&snapshot.manifest.store_id, op)?;
            out.push(PositionedEvent {
                cursor: format!("ga:{}:legacy:{:020}:000000", snapshot.root, entry.ordinal),
                phase: "legacy",
                position: json!({"ordinal":entry.ordinal,"event_index":0}),
                event: serde_json::to_value(event)?,
                matches,
            });
        }
    }
    for (path, bytes) in &snapshot.files {
        if !path.starts_with("transactions/") {
            continue;
        }
        let tx: protocol::Transaction = protocol::decode(bytes)?;
        for effect in tx.effects {
            let (event, matches) = match &effect {
                protocol::Effect::TaskOp { op, .. }
                | protocol::Effect::CommunicationOp { op, .. } => {
                    let op: Op = serde_json::from_value(op.clone())?;
                    let matches = filter.matches(&op, &domain);
                    (
                        serde_json::to_value(event_from_op(&snapshot.manifest.store_id, op)?)?,
                        matches,
                    )
                }
                protocol::Effect::Ownership { transition, .. } => {
                    let (category, event_type) = ownership_kind(transition);
                    let data = serde_json::to_value(transition)?;
                    let mut actors = BTreeSet::from([tx.request.actor.as_str()]);
                    if let Some(object) = data.as_object() {
                        for (key, value) in object {
                            if key == "actor" || key.ends_with("_actor") {
                                if let Some(actor) = value.as_str() {
                                    actors.insert(actor);
                                }
                            }
                        }
                    }
                    let matches = actors
                        .into_iter()
                        .any(|actor| filter.matches_projection(category, actor));
                    (
                        json!({"schema":"mote.event.v1","event_id":effect.effect_id(),"store_id":snapshot.manifest.store_id,"type":event_type,"category":category,"op_id":effect.adapter_id(),"ts":tx.request.timestamp,"actor":tx.request.actor,"accepted":true,"data":data}),
                        matches,
                    )
                }
                protocol::Effect::SyntheticSet { .. } => continue,
            };
            out.push(PositionedEvent {
                cursor:format!("ga:{}:remote:{:020}:{:06}",snapshot.root,tx.sequence,effect.index()),
                phase:"remote",position:json!({"sequence":tx.sequence,"effect_index":effect.index(),"request_id":tx.request.request_id}),event,matches
            });
        }
    }
    Ok(out)
}

fn ownership_kind(transition: &Transition) -> (&'static str, &'static str) {
    match transition {
        Transition::AuthorityRetired(_) => ("session", "authority.retired"),
        Transition::SessionStarted { .. } => ("session", "session.started"),
        Transition::SessionPresence { .. } => ("presence", "session.presence"),
        Transition::SessionEnded(_) => ("session", "session.ended"),
        Transition::SessionRevoked(_) => ("session", "session.revoked"),
        Transition::ClaimAcquired { .. } => ("claim", "claim.acquired"),
        Transition::ClaimReleased { .. } => ("claim", "claim.released"),
        Transition::ClaimTransferred(_) => ("claim", "claim.transferred"),
        Transition::ClaimRecovered(_) => ("claim", "claim.recovered"),
        Transition::ReservationAcquired { .. } => ("reservation", "reservation.acquired"),
        Transition::ReservationReleased { .. } => ("reservation", "reservation.released"),
        Transition::ReservationTransferred(_) => ("reservation", "reservation.transferred"),
        Transition::ReservationRecovered(_) => ("reservation", "reservation.recovered"),
    }
}

fn invalid<T>(message: &str) -> MoteResult<T> {
    Err(MoteError::Invalid(message.into()))
}
