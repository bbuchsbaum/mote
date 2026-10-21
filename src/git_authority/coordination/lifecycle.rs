//! Compound lifecycle contract for the coordination profile.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Disposition {
    Release,
    Transfer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Begin {
    pub id: String,
    pub expect_status: String,
    #[serde(deserialize_with = "required_option")]
    pub reservation: Option<String>,
    pub paths: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub note: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Next {
    pub tags: Vec<String>,
    pub exclude: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub reservation: Option<String>,
    pub paths: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub note: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Done {
    pub id: String,
    pub expect_status: String,
    pub expect_claim: String,
    pub reservations: BTreeMap<String, String>,
    pub note: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handoff {
    pub id: String,
    pub expect_status: String,
    pub expect_claim: String,
    pub reservations: BTreeMap<String, String>,
    pub to_session: String,
    pub disposition: Disposition,
    pub note: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnd {
    pub expect_session: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecover {
    pub source_session: String,
    pub expect_session: String,
    pub claims: BTreeMap<String, String>,
    pub reservations: BTreeMap<String, String>,
    pub disposition: Disposition,
    #[serde(deserialize_with = "required_option")]
    pub recipient_session: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimTransferred {
    pub task: String,
    pub from_actor: String,
    pub from_session: String,
    pub old_token: String,
    pub to_actor: String,
    pub to_session: String,
    pub token: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationTransferred {
    pub reservation: String,
    pub task: String,
    pub paths: Vec<String>,
    pub from_actor: String,
    pub from_session: String,
    pub old_token: String,
    pub to_actor: String,
    pub to_session: String,
    pub token: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionEnded {
    pub session: String,
    pub actor: String,
    pub token: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimRecovered {
    pub task: String,
    pub requester_actor: String,
    pub requester_session: String,
    pub source_actor: String,
    pub source_session: String,
    pub old_token: String,
    pub disposition: Disposition,
    #[serde(deserialize_with = "required_option")]
    pub new_actor: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub new_session: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub new_token: Option<String>,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationRecovered {
    pub reservation: String,
    pub task: String,
    pub paths: Vec<String>,
    pub requester_actor: String,
    pub requester_session: String,
    pub source_actor: String,
    pub source_session: String,
    pub old_token: String,
    pub disposition: Disposition,
    #[serde(deserialize_with = "required_option")]
    pub new_actor: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub new_session: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub new_token: Option<String>,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRevoked {
    pub session: String,
    pub actor: String,
    pub token: String,
    pub requester_actor: String,
    pub requester_session: String,
    pub disposition: Disposition,
    #[serde(deserialize_with = "required_option")]
    pub new_actor: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub new_session: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub new_token: Option<String>,
    pub reason: String,
}

fn effect_id(request: &Request, index: usize) -> String {
    format!("eff-{}-{index:06}", request.request_id)
}

fn push_ownership(
    effects: &mut Vec<Effect>,
    request: &Request,
    sequence: u64,
    transition: Transition,
) {
    let index = effects.len();
    effects.push(Effect::Ownership {
        effect_id: effect_id(request, index),
        sequence,
        index: index as u32,
        transition: Box::new(transition),
    });
}

fn push_task(effects: &mut Vec<Effect>, request: &Request, sequence: u64, body: Value) {
    let index = effects.len();
    let id = effect_id(request, index);
    effects.push(super::task_effect_at(
        &id,
        sequence,
        index as u32,
        request,
        body,
    ));
}

fn token(v: &str) -> MoteResult<()> {
    if v.is_empty() || v.len() > 256 || !v.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return Err(MoteError::Invalid("invalid lifecycle token".into()));
    };
    Ok(())
}
fn reason(v: &str) -> MoteResult<()> {
    if v.is_empty() || v.len() > 4096 || v.trim().is_empty() {
        return Err(MoteError::Invalid("invalid lifecycle reason".into()));
    };
    Ok(())
}
fn check_paths(reservation: &Option<String>, paths: &[String]) -> MoteResult<()> {
    match reservation {
        Some(id) => {
            reservation_id(id)?;
            paths_ok(paths)
        }
        None if paths.is_empty() => Ok(()),
        None => Err(MoteError::Invalid("paths require reservation".into())),
    }
}
fn check_session(v: &str) -> MoteResult<()> {
    if v.trim().is_empty() || v.len() > 128 || v.chars().any(char::is_control) {
        return Err(MoteError::Invalid("invalid session".into()));
    };
    Ok(())
}

pub fn validate(action: &Action) -> MoteResult<()> {
    let note = |v: &Option<String>| -> MoteResult<()> {
        if let Some(n) = v {
            if n.is_empty() || n.len() > 1 << 20 {
                return Err(MoteError::Invalid("invalid lifecycle note".into()));
            }
        };
        Ok(())
    };
    match action {
        Action::Begin(v) => {
            task_id(&v.id)?;
            token(&v.expect_status)?;
            check_paths(&v.reservation, &v.paths)?;
            note(&v.note)
        }
        Action::Next(v) => {
            if v.tags.len() > 64
                || v.exclude.len() > 64
                || !v.tags.windows(2).all(|w| w[0] < w[1])
                || !v.exclude.windows(2).all(|w| w[0] < w[1])
            {
                return Err(MoteError::Invalid("invalid next filters".into()));
            };
            for t in &v.tags {
                nonblank(t, 128, "tag")?
            }
            for id in &v.exclude {
                task_id(id)?
            }
            check_paths(&v.reservation, &v.paths)?;
            note(&v.note)
        }
        Action::Done(v) => {
            task_id(&v.id)?;
            token(&v.expect_status)?;
            token(&v.expect_claim)?;
            if v.note.is_empty() || v.note.len() > 1 << 20 {
                return Err(MoteError::Invalid("invalid lifecycle note".into()));
            };
            for (k, t) in &v.reservations {
                reservation_id(k)?;
                token(t)?
            }
            Ok(())
        }
        Action::Handoff(v) => {
            task_id(&v.id)?;
            token(&v.expect_status)?;
            token(&v.expect_claim)?;
            check_session(&v.to_session)?;
            if v.note.is_empty() || v.note.len() > 1 << 20 {
                return Err(MoteError::Invalid("invalid lifecycle note".into()));
            };
            for (k, t) in &v.reservations {
                reservation_id(k)?;
                token(t)?
            }
            Ok(())
        }
        Action::SessionEnd(v) => {
            token(&v.expect_session)?;
            reason(&v.reason)
        }
        Action::SessionRecover(v) => {
            check_session(&v.source_session)?;
            token(&v.expect_session)?;
            reason(&v.reason)?;
            match (&v.disposition, &v.recipient_session) {
                (Disposition::Release, None) | (Disposition::Transfer, Some(_)) => (),
                _ => return Err(MoteError::Invalid("invalid recovery recipient".into())),
            };
            if let Some(s) = &v.recipient_session {
                check_session(s)?
            };
            for (k, t) in &v.claims {
                task_id(k)?;
                token(t)?
            }
            for (k, t) in &v.reservations {
                reservation_id(k)?;
                token(t)?
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub fn plan(
    state: &CoordinationState,
    request: &Request,
    sequence: u64,
) -> MoteResult<CoordinationPlan> {
    request.validate()?;
    let action = match &request.command {
        protocol::Command::Coordination { action } => action,
        _ => return Err(MoteError::Invalid("inactive command profile".into())),
    };
    let reject = |code: &str, subject: &str| CoordinationPlan {
        effects: vec![],
        outcome: Outcome {
            status: OutcomeStatus::Rejected,
            result: json!({"code":code,"subject":subject}),
        },
        next_state: state.clone(),
    };
    let active = |session: &str| state.sessions.get(session).filter(|s| s.status == "active");
    let Some(requester) = active(&request.session) else {
        return Ok(reject("session_unavailable", &request.session));
    };
    if requester.actor != request.actor {
        return Ok(reject("session_unavailable", &request.session));
    }
    let tasks = replay_tasks(state)?;
    let mut next = state.clone();
    let mut effects = Vec::new();
    let active_holdings = |session: &str| -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        (
            state
                .claims
                .iter()
                .filter(|(_, c)| c.session == session)
                .map(|(k, c)| (k.clone(), c.token.clone()))
                .collect(),
            state
                .reservations
                .iter()
                .filter(|(_, r)| r.status == "open" && r.session == session)
                .map(|(k, r)| (k.clone(), r.token.clone()))
                .collect(),
        )
    };
    let deps_satisfied = |t: &Task| {
        t.deps
            .iter()
            .all(|id| tasks.get(id).is_none_or(|p| p.status == "closed"))
    };
    let eligible = |t: &Task| matches!(t.status.as_str(), "open" | "doing") && deps_satisfied(t);
    let exact_map = |actual: &BTreeMap<String, String>, supplied: &BTreeMap<String, String>| {
        actual
            .keys()
            .chain(supplied.keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .find(|k| actual.get(*k) != supplied.get(*k))
            .cloned()
    };
    let finish = |mut next: CoordinationState,
                  effects: Vec<Effect>,
                  subject: String,
                  result: Value|
     -> MoteResult<CoordinationPlan> {
        for effect in &effects {
            if let Effect::TaskOp { op, .. } = effect {
                next.task_entries.push(op.clone());
                if replay_tasks(&next).is_err() {
                    return Ok(reject("task_effect_rejected", &subject));
                }
            }
        }
        Ok(CoordinationPlan {
            effects: effects.clone(),
            outcome: Outcome {
                status: OutcomeStatus::Accepted,
                result,
            },
            next_state: next,
        })
    };
    match action {
        Action::Begin(v) => {
            let Some(t) = tasks.get(&v.id) else {
                return Ok(reject("task_unavailable", &v.id));
            };
            if t.clock.get("status") != Some(&v.expect_status) {
                return Ok(reject("stale_field", "status"));
            }
            if !eligible(t) {
                return Ok(reject("task_unavailable", &v.id));
            }
            if state.claims.contains_key(&v.id) {
                return Ok(reject("claim_occupied", &v.id));
            }
            if let Some(rid) = &v.reservation {
                if state.reservations.contains_key(rid) || state.consumed_reservation(rid) {
                    return Ok(reject("reservation_exists", rid));
                }
                if let Some((id, _)) = state
                    .reservations
                    .iter()
                    .find(|(_, r)| r.status == "open" && overlap(&r.paths, &v.paths))
                {
                    return Ok(reject("path_conflict", id));
                }
                let token = effect_id(request, effects.len());
                next.reservations.insert(
                    rid.clone(),
                    Reservation {
                        task: v.id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                        paths: v.paths.clone(),
                        status: "open".into(),
                    },
                );
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ReservationAcquired {
                        reservation: rid.clone(),
                        task: v.id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token,
                        paths: v.paths.clone(),
                    },
                );
            }
            let claim_token = effect_id(request, effects.len());
            next.claims.insert(
                v.id.clone(),
                Claim {
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: claim_token.clone(),
                },
            );
            push_ownership(
                &mut effects,
                request,
                sequence,
                Transition::ClaimAcquired {
                    task: v.id.clone(),
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: claim_token.clone(),
                },
            );
            if t.status == "open" {
                push_task(
                    &mut effects,
                    request,
                    sequence,
                    json!({"kind":"patch","entity":v.id,"set":{"status":"doing"},"expect":{"status":v.expect_status}}),
                );
            }
            if let Some(note) = &v.note {
                push_task(
                    &mut effects,
                    request,
                    sequence,
                    json!({"kind":"note","entity":v.id,"note_kind":"progress","text":note}),
                );
            }
            let reservation = v
                .reservation
                .as_ref()
                .map(|id| json!({"id":id,"token":next.reservations[id].token,"paths":v.paths}));
            finish(
                next,
                effects.clone(),
                v.id.clone(),
                json!({"code":"ok","subject":v.id,"effect_ids":effects.iter().map(|x|x.effect_id()).collect::<Vec<_>>(),"task":v.id,"claim_token":claim_token,"reservation":reservation}),
            )
        }
        Action::Next(v) => {
            let selected = tasks
                .iter()
                .filter(|(id, t)| {
                    !v.exclude.contains(id)
                        && v.tags.iter().all(|tag| t.tags.contains(tag))
                        && eligible(t)
                        && !state.claims.contains_key(*id)
                        && !state
                            .reservations
                            .values()
                            .any(|r| r.status == "open" && r.task == **id)
                })
                .min_by_key(|(id, t)| (t.priority, (*id).clone()));
            let Some((id, t)) = selected else {
                return Ok(reject("no_ready_task", "next"));
            };
            let id = id.clone();
            if let Some(rid) = &v.reservation {
                if state.reservations.contains_key(rid) || state.consumed_reservation(rid) {
                    return Ok(reject("reservation_exists", rid));
                }
                if let Some((rid, _)) = state
                    .reservations
                    .iter()
                    .find(|(_, r)| r.status == "open" && overlap(&r.paths, &v.paths))
                {
                    return Ok(reject("path_conflict", rid));
                }
                let token = effect_id(request, effects.len());
                next.reservations.insert(
                    rid.clone(),
                    Reservation {
                        task: id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                        paths: v.paths.clone(),
                        status: "open".into(),
                    },
                );
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ReservationAcquired {
                        reservation: rid.clone(),
                        task: id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token,
                        paths: v.paths.clone(),
                    },
                );
            }
            let claim_token = effect_id(request, effects.len());
            next.claims.insert(
                id.clone(),
                Claim {
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: claim_token.clone(),
                },
            );
            push_ownership(
                &mut effects,
                request,
                sequence,
                Transition::ClaimAcquired {
                    task: id.clone(),
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: claim_token.clone(),
                },
            );
            if t.status == "open" {
                let clock = t.clock.get("status").cloned().unwrap_or_default();
                push_task(
                    &mut effects,
                    request,
                    sequence,
                    json!({"kind":"patch","entity":id,"set":{"status":"doing"},"expect":{"status":clock}}),
                );
            }
            if let Some(note) = &v.note {
                push_task(
                    &mut effects,
                    request,
                    sequence,
                    json!({"kind":"note","entity":id,"note_kind":"progress","text":note}),
                );
            }
            let reservation = v
                .reservation
                .as_ref()
                .map(|rid| json!({"id":rid,"token":next.reservations[rid].token,"paths":v.paths}));
            finish(
                next,
                effects.clone(),
                id.clone(),
                json!({"code":"ok","subject":id,"effect_ids":effects.iter().map(|x|x.effect_id()).collect::<Vec<_>>(),"task":id,"claim_token":claim_token,"reservation":reservation}),
            )
        }
        Action::Done(v) => {
            let Some(t) = tasks.get(&v.id) else {
                return Ok(reject("task_unavailable", &v.id));
            };
            if t.status == "closed" {
                return Ok(reject("task_unavailable", &v.id));
            }
            if t.clock.get("status") != Some(&v.expect_status) {
                return Ok(reject("stale_field", "status"));
            }
            match state.claims.get(&v.id) {
                Some(c)
                    if c.actor == request.actor
                        && c.session == request.session
                        && c.token == v.expect_claim => {}
                _ => return Ok(reject("claim_mismatch", &v.id)),
            };
            let actual: BTreeMap<_, _> = state
                .reservations
                .iter()
                .filter(|(_, r)| {
                    r.status == "open" && r.session == request.session && r.task == v.id
                })
                .map(|(k, r)| (k.clone(), r.token.clone()))
                .collect();
            if let Some(k) = exact_map(&actual, &v.reservations) {
                return Ok(reject("reservation_mismatch", &k));
            };
            if state
                .reservations
                .values()
                .any(|r| r.status == "open" && r.task == v.id && r.session != request.session)
            {
                return Ok(reject("held_work", &v.id));
            }
            push_task(
                &mut effects,
                request,
                sequence,
                json!({"kind":"patch","entity":v.id,"set":{"status":"closed"},"expect":{"status":v.expect_status}}),
            );
            push_task(
                &mut effects,
                request,
                sequence,
                json!({"kind":"note","entity":v.id,"note_kind":"note","text":v.note}),
            );
            for (rid, token) in &v.reservations {
                let r = next.reservations.get_mut(rid).unwrap();
                r.status = "closed".into();
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ReservationReleased {
                        reservation: rid.clone(),
                        task: v.id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                    },
                );
            }
            next.claims.remove(&v.id);
            push_ownership(
                &mut effects,
                request,
                sequence,
                Transition::ClaimReleased {
                    task: v.id.clone(),
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: v.expect_claim.clone(),
                },
            );
            let released = v
                .reservations
                .iter()
                .map(|(id, t)| json!({"id":id,"token":t,"paths":state.reservations[id].paths}))
                .collect::<Vec<_>>();
            finish(
                next,
                effects.clone(),
                v.id.clone(),
                json!({"code":"ok","subject":v.id,"effect_ids":effects.iter().map(|x|x.effect_id()).collect::<Vec<_>>(),"task":v.id,"released_claim":v.expect_claim,"released_reservations":released}),
            )
        }
        Action::Handoff(v) => {
            let Some(t) = tasks.get(&v.id) else {
                return Ok(reject("task_unavailable", &v.id));
            };
            if t.status == "closed" {
                return Ok(reject("task_unavailable", &v.id));
            }
            if t.clock.get("status") != Some(&v.expect_status) {
                return Ok(reject("stale_field", "status"));
            };
            match state.claims.get(&v.id) {
                Some(c)
                    if c.actor == request.actor
                        && c.session == request.session
                        && c.token == v.expect_claim => {}
                _ => return Ok(reject("claim_mismatch", &v.id)),
            };
            let Some(recipient) = active(&v.to_session) else {
                return Ok(reject("session_unavailable", &v.to_session));
            };
            if v.to_session == request.session {
                return Ok(reject("self_target", &v.to_session));
            };
            let actual: BTreeMap<_, _> = state
                .reservations
                .iter()
                .filter(|(_, r)| {
                    r.status == "open" && r.session == request.session && r.task == v.id
                })
                .map(|(k, r)| (k.clone(), r.token.clone()))
                .collect();
            if let Some(k) = exact_map(&actual, &v.reservations) {
                return Ok(reject("reservation_mismatch", &k));
            }
            push_task(
                &mut effects,
                request,
                sequence,
                json!({"kind":"note","entity":v.id,"note_kind":"handoff","text":v.note}),
            );
            let claim_token = effect_id(request, effects.len());
            next.claims.insert(
                v.id.clone(),
                Claim {
                    actor: recipient.actor.clone(),
                    session: v.to_session.clone(),
                    token: claim_token.clone(),
                },
            );
            push_ownership(
                &mut effects,
                request,
                sequence,
                Transition::ClaimTransferred(ClaimTransferred {
                    task: v.id.clone(),
                    from_actor: request.actor.clone(),
                    from_session: request.session.clone(),
                    old_token: v.expect_claim.clone(),
                    to_actor: recipient.actor.clone(),
                    to_session: v.to_session.clone(),
                    token: claim_token.clone(),
                }),
            );
            for (rid, old) in &v.reservations {
                let r = next.reservations.get_mut(rid).unwrap();
                let paths = r.paths.clone();
                let new_token = match v.disposition {
                    Disposition::Release => {
                        r.status = "closed".into();
                        push_ownership(
                            &mut effects,
                            request,
                            sequence,
                            Transition::ReservationReleased {
                                reservation: rid.clone(),
                                task: v.id.clone(),
                                actor: request.actor.clone(),
                                session: request.session.clone(),
                                token: old.clone(),
                            },
                        );
                        None
                    }
                    Disposition::Transfer => {
                        let tok = effect_id(request, effects.len());
                        r.actor = recipient.actor.clone();
                        r.session = v.to_session.clone();
                        r.token = tok.clone();
                        push_ownership(
                            &mut effects,
                            request,
                            sequence,
                            Transition::ReservationTransferred(ReservationTransferred {
                                reservation: rid.clone(),
                                task: v.id.clone(),
                                paths,
                                from_actor: request.actor.clone(),
                                from_session: request.session.clone(),
                                old_token: old.clone(),
                                to_actor: recipient.actor.clone(),
                                to_session: v.to_session.clone(),
                                token: tok.clone(),
                            }),
                        );
                        Some(tok)
                    }
                };
                let _ = new_token;
            }
            let reservations=v.reservations.iter().map(|(rid,old)|{let r=&next.reservations[rid];json!({"id":rid,"paths":r.paths,"previous_token":old,"token":if matches!(v.disposition,Disposition::Transfer){Value::String(r.token.clone())}else{Value::Null}})}).collect::<Vec<_>>();
            let other=state.reservations.iter().filter(|(_,r)|r.status=="open"&&r.task==v.id&&r.session!=request.session).map(|(id,r)|json!({"id":id,"task":r.task,"actor":r.actor,"session":r.session,"token":r.token,"paths":r.paths})).collect::<Vec<_>>();
            finish(
                next,
                effects.clone(),
                v.id.clone(),
                json!({"code":"ok","subject":v.id,"effect_ids":effects.iter().map(|x|x.effect_id()).collect::<Vec<_>>(),"task":v.id,"disposition":v.disposition,"recipient_session":v.to_session,"claim_token":claim_token,"reservations":reservations,"unaffected_other_session_reservations":other}),
            )
        }
        Action::SessionEnd(v) => {
            if requester.token != v.expect_session {
                return Ok(reject("enrollment_mismatch", &request.session));
            }
            let (claims, reservations) = active_holdings(&request.session);
            for (id, token) in &claims {
                next.claims.remove(id);
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ClaimReleased {
                        task: id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                    },
                );
            }
            for (id, token) in &reservations {
                let r = next.reservations.get_mut(id).unwrap();
                let task_id = r.task.clone();
                r.status = "closed".into();
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ReservationReleased {
                        reservation: id.clone(),
                        task: task_id,
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                    },
                );
            }
            next.sessions.get_mut(&request.session).unwrap().status = "ended".into();
            push_ownership(
                &mut effects,
                request,
                sequence,
                Transition::SessionEnded(SessionEnded {
                    session: request.session.clone(),
                    actor: request.actor.clone(),
                    token: v.expect_session.clone(),
                    reason: v.reason.clone(),
                }),
            );
            let rc = claims
                .iter()
                .map(|(task, token)| json!({"task":task,"token":token}))
                .collect::<Vec<_>>();
            let rr=reservations.iter().map(|(id,token)|json!({"id":id,"task":state.reservations[id].task,"token":token,"paths":state.reservations[id].paths})).collect::<Vec<_>>();
            finish(
                next,
                effects.clone(),
                request.session.clone(),
                json!({"code":"ok","subject":request.session,"effect_ids":effects.iter().map(|x|x.effect_id()).collect::<Vec<_>>(),"session":request.session,"released_claims":rc,"released_reservations":rr}),
            )
        }
        Action::SessionRecover(v) => {
            let Some(source) = active(&v.source_session) else {
                return Ok(reject("session_unavailable", &v.source_session));
            };
            if v.source_session == request.session {
                return Ok(reject("self_target", &v.source_session));
            }
            if source.token != v.expect_session {
                return Ok(reject("enrollment_mismatch", &v.source_session));
            }
            let recipient = match (&v.disposition, &v.recipient_session) {
                (Disposition::Release, None) => None,
                (Disposition::Transfer, Some(s)) => {
                    let Some(r) = active(s) else {
                        return Ok(reject("session_unavailable", s));
                    };
                    if s == &v.source_session {
                        return Ok(reject("self_target", s));
                    }
                    Some(r)
                }
                _ => unreachable!(),
            };
            let (actual_claims, actual_reservations) = active_holdings(&v.source_session);
            if let Some(k) = exact_map(&actual_claims, &v.claims) {
                return Ok(reject("claim_mismatch", &k));
            }
            if let Some(k) = exact_map(&actual_reservations, &v.reservations) {
                return Ok(reject("reservation_mismatch", &k));
            }
            for (id, old) in &v.claims {
                let (new_actor, new_session, new_token) = if let Some(r) = recipient {
                    let tok = effect_id(request, effects.len());
                    next.claims.insert(
                        id.clone(),
                        Claim {
                            actor: r.actor.clone(),
                            session: v.recipient_session.clone().unwrap(),
                            token: tok.clone(),
                        },
                    );
                    (
                        Some(r.actor.clone()),
                        v.recipient_session.clone(),
                        Some(tok),
                    )
                } else {
                    next.claims.remove(id);
                    (None, None, None)
                };
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ClaimRecovered(ClaimRecovered {
                        task: id.clone(),
                        requester_actor: request.actor.clone(),
                        requester_session: request.session.clone(),
                        source_actor: source.actor.clone(),
                        source_session: v.source_session.clone(),
                        old_token: old.clone(),
                        disposition: v.disposition.clone(),
                        new_actor,
                        new_session,
                        new_token,
                        reason: v.reason.clone(),
                    }),
                );
            }
            for (id, old) in &v.reservations {
                let r = next.reservations.get_mut(id).unwrap();
                let task = r.task.clone();
                let paths = r.paths.clone();
                let (new_actor, new_session, new_token) = if let Some(to) = recipient {
                    let tok = effect_id(request, effects.len());
                    r.actor = to.actor.clone();
                    r.session = v.recipient_session.clone().unwrap();
                    r.token = tok.clone();
                    (
                        Some(to.actor.clone()),
                        v.recipient_session.clone(),
                        Some(tok),
                    )
                } else {
                    r.status = "closed".into();
                    (None, None, None)
                };
                push_ownership(
                    &mut effects,
                    request,
                    sequence,
                    Transition::ReservationRecovered(ReservationRecovered {
                        reservation: id.clone(),
                        task,
                        paths,
                        requester_actor: request.actor.clone(),
                        requester_session: request.session.clone(),
                        source_actor: source.actor.clone(),
                        source_session: v.source_session.clone(),
                        old_token: old.clone(),
                        disposition: v.disposition.clone(),
                        new_actor,
                        new_session,
                        new_token,
                        reason: v.reason.clone(),
                    }),
                );
            }
            next.sessions.get_mut(&v.source_session).unwrap().status = "revoked".into();
            let (na, ns, nt) = if let Some(r) = recipient {
                (
                    Some(r.actor.clone()),
                    v.recipient_session.clone(),
                    Some(r.token.clone()),
                )
            } else {
                (None, None, None)
            };
            push_ownership(
                &mut effects,
                request,
                sequence,
                Transition::SessionRevoked(SessionRevoked {
                    session: v.source_session.clone(),
                    actor: source.actor.clone(),
                    token: v.expect_session.clone(),
                    requester_actor: request.actor.clone(),
                    requester_session: request.session.clone(),
                    disposition: v.disposition.clone(),
                    new_actor: na,
                    new_session: ns,
                    new_token: nt,
                    reason: v.reason.clone(),
                }),
            );
            let cs=v.claims.iter().map(|(task,old)|json!({"task":task,"previous_token":old,"token":next.claims.get(task).map(|c|Value::String(c.token.clone())).unwrap_or(Value::Null)})).collect::<Vec<_>>();
            let rs=v.reservations.iter().map(|(id,old)|{let r=&next.reservations[id];json!({"id":id,"task":r.task,"paths":r.paths,"previous_token":old,"token":if r.status=="open"{Value::String(r.token.clone())}else{Value::Null}})}).collect::<Vec<_>>();
            let touched: std::collections::BTreeSet<String> = v
                .claims
                .keys()
                .cloned()
                .chain(
                    v.reservations
                        .keys()
                        .map(|id| state.reservations[id].task.clone()),
                )
                .collect();
            let other=state.reservations.iter().filter(|(_,r)| r.status=="open" && r.session!=v.source_session && touched.contains(&r.task)).map(|(id,r)|json!({"id":id,"task":r.task,"actor":r.actor,"session":r.session,"token":r.token,"paths":r.paths})).collect::<Vec<_>>();
            finish(
                next,
                effects.clone(),
                v.source_session.clone(),
                json!({"code":"ok","subject":v.source_session,"effect_ids":effects.iter().map(|x|x.effect_id()).collect::<Vec<_>>(),"source_session":v.source_session,"disposition":v.disposition,"recipient_session":v.recipient_session,"claims":cs,"reservations":rs,"unaffected_other_session_reservations":other}),
            )
        }
        _ => Err(MoteError::Invalid("not a lifecycle action".into())),
    }
}
