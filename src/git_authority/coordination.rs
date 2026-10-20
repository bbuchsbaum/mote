//! Pure coordination-core planning.  This deliberately has no clocks, IO, or
//! local publication calls: canonical effect positions are supplied by replay.
use super::protocol::{self, Effect, Outcome, OutcomeStatus, Request};
use crate::{MoteError, MoteResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub mod lifecycle;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Action {
    #[serde(rename = "session.start")]
    SessionStart { label: String },
    #[serde(rename = "session.presence")]
    SessionPresence { text: String },
    #[serde(rename = "task.create")]
    TaskCreate {
        id: String,
        title: String,
        body: String,
        priority: i32,
        #[serde(deserialize_with = "required_option")]
        assignee: Option<String>,
    },
    #[serde(rename = "task.patch")]
    TaskPatch {
        id: String,
        set: BTreeMap<String, Value>,
        expect: BTreeMap<String, Option<String>>,
    },
    #[serde(rename = "task.tag")]
    TaskTag { id: String, tag: String, add: bool },
    #[serde(rename = "task.dependency")]
    TaskDependency {
        id: String,
        parent: String,
        add: bool,
    },
    #[serde(rename = "task.relation")]
    TaskRelation {
        id: String,
        parent: String,
        relation: String,
        add: bool,
    },
    #[serde(rename = "task.note")]
    TaskNote {
        id: String,
        note_kind: String,
        text: String,
    },
    #[serde(rename = "claim.acquire")]
    ClaimAcquire { id: String },
    #[serde(rename = "claim.release")]
    ClaimRelease { id: String, token: String },
    #[serde(rename = "reservation.acquire")]
    ReservationAcquire {
        id: String,
        reservation: String,
        paths: Vec<String>,
    },
    #[serde(rename = "reservation.release")]
    ReservationRelease { reservation: String, token: String },
    #[serde(rename = "begin")]
    Begin(lifecycle::Begin),
    #[serde(rename = "next")]
    Next(lifecycle::Next),
    #[serde(rename = "done")]
    Done(lifecycle::Done),
    #[serde(rename = "handoff")]
    Handoff(lifecycle::Handoff),
    #[serde(rename = "session.end")]
    SessionEnd(lifecycle::SessionEnd),
    #[serde(rename = "session.recover")]
    SessionRecover(lifecycle::SessionRecover),
}

impl Action {
    pub(crate) fn is_lifecycle(&self) -> bool {
        matches!(
            self,
            Self::Begin(_)
                | Self::Next(_)
                | Self::Done(_)
                | Self::Handoff(_)
                | Self::SessionEnd(_)
                | Self::SessionRecover(_)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transition {
    SessionStarted {
        session: String,
        actor: String,
        token: String,
        label: String,
        timestamp: String,
    },
    SessionPresence {
        session: String,
        actor: String,
        text: String,
        timestamp: String,
    },
    ClaimAcquired {
        task: String,
        actor: String,
        session: String,
        token: String,
    },
    ClaimReleased {
        task: String,
        actor: String,
        session: String,
        token: String,
    },
    ReservationAcquired {
        reservation: String,
        task: String,
        actor: String,
        session: String,
        token: String,
        paths: Vec<String>,
    },
    ReservationReleased {
        reservation: String,
        task: String,
        actor: String,
        session: String,
        token: String,
    },
    ClaimTransferred(lifecycle::ClaimTransferred),
    ReservationTransferred(lifecycle::ReservationTransferred),
    SessionEnded(lifecycle::SessionEnded),
    ClaimRecovered(lifecycle::ClaimRecovered),
    ReservationRecovered(lifecycle::ReservationRecovered),
    SessionRevoked(lifecycle::SessionRevoked),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub actor: String,
    pub token: String,
    pub label: String,
    pub status: String,
    pub presence_timestamp: String,
    pub presence_text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub actor: String,
    pub session: String,
    pub token: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reservation {
    pub task: String,
    pub actor: String,
    pub session: String,
    pub token: String,
    pub paths: Vec<String>,
    pub status: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Task {
    title: String,
    body: String,
    priority: i32,
    assignee: Option<String>,
    status: String,
    clock: BTreeMap<String, String>,
    tags: BTreeSet<String>,
    deps: BTreeSet<String>,
    rels: BTreeSet<(String, String)>,
    notes: Vec<TaskNote>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct TaskNote {
    op_id: String,
    note_kind: String,
    actor: String,
    ts: String,
    text: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinationState {
    sessions: BTreeMap<String, Session>,
    claims: BTreeMap<String, Claim>,
    reservations: BTreeMap<String, Reservation>,
    task_entries: Vec<Value>,
}
fn required_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinationPlan {
    pub effects: Vec<Effect>,
    pub outcome: Outcome,
    pub next_state: CoordinationState,
}

pub fn validate_action(a: &Action) -> MoteResult<()> {
    match a {
        Action::Begin(_)
        | Action::Next(_)
        | Action::Done(_)
        | Action::Handoff(_)
        | Action::SessionEnd(_)
        | Action::SessionRecover(_) => lifecycle::validate(a)?,
        Action::SessionStart { label } | Action::SessionPresence { text: label } => {
            text(label, 4096, "session text")?
        }
        Action::TaskCreate {
            id,
            title,
            body,
            priority,
            assignee,
        } => {
            task_id(id)?;
            if title.is_empty() {
                return bad("invalid title");
            }
            text(title, 4096, "title")?;
            text(body, 1 << 20, "body")?;
            if *priority < 0 || *priority > 3 {
                return bad("invalid priority");
            };
            if let Some(v) = assignee {
                nonblank(v, 128, "assignee")?;
            }
        }
        Action::TaskPatch { id, set, expect } => {
            task_id(id)?;
            if set.is_empty()
                || set.len() != expect.len()
                || set
                    .keys()
                    .any(|k| !matches!(k.as_str(), "title" | "body" | "priority" | "assignee"))
                || set.keys().any(|k| !expect.contains_key(k))
            {
                return bad("invalid patch");
            };
            for (k, v) in set {
                validate_field(k, v)?;
            }
        }
        Action::TaskTag { id, tag, .. } => {
            task_id(id)?;
            nonblank(tag, 128, "tag")?
        }
        Action::TaskDependency { id, parent, .. } => {
            task_id(id)?;
            task_id(parent)?
        }
        Action::TaskRelation {
            id,
            parent,
            relation,
            ..
        } => {
            task_id(id)?;
            task_id(parent)?;
            if relation != "parent" && relation != "related" {
                return bad("invalid relation");
            }
        }
        Action::TaskNote {
            id,
            note_kind,
            text: body,
        } => {
            task_id(id)?;
            if !matches!(
                note_kind.as_str(),
                "note" | "progress" | "decision" | "design" | "handoff" | "blocker"
            ) {
                return bad("invalid note kind");
            }
            text(body, 1 << 20, "note text")?
        }
        Action::ClaimAcquire { id } | Action::ClaimRelease { id, .. } => task_id(id)?,
        Action::ReservationAcquire {
            id,
            reservation,
            paths,
        } => {
            task_id(id)?;
            reservation_id(reservation)?;
            paths_ok(paths)?
        }
        Action::ReservationRelease { reservation, .. } => reservation_id(reservation)?,
    };
    Ok(())
}
fn bad<T>(s: &str) -> MoteResult<T> {
    Err(MoteError::Invalid(s.into()))
}
fn text(s: &str, n: usize, w: &str) -> MoteResult<()> {
    if s.len() > n {
        return bad(&format!("invalid {w}"));
    };
    Ok(())
}
fn nonblank(s: &str, n: usize, w: &str) -> MoteResult<()> {
    if s.trim().is_empty() || s.len() > n || s.chars().any(char::is_control) {
        return bad(&format!("invalid {w}"));
    };
    Ok(())
}
fn task_id(s: &str) -> MoteResult<()> {
    if !s.starts_with("bd-") || !protocol::simple_identifier(&s[3..], 100) {
        return bad("invalid task ID");
    };
    Ok(())
}
fn reservation_id(s: &str) -> MoteResult<()> {
    if !s.starts_with("rv-") || !protocol::simple_identifier(&s[3..], 100) {
        return bad("invalid reservation ID");
    };
    Ok(())
}
fn paths_ok(paths: &[String]) -> MoteResult<()> {
    if paths.is_empty()
        || paths.len() > 64
        || !paths.windows(2).all(|w| w[0] < w[1])
        || paths.iter().any(|p| {
            p.contains('\\')
                || p.chars().any(char::is_control)
                || crate::paths::normalize(p).as_deref() != Ok(p.as_str())
        })
    {
        return bad("invalid reservation paths");
    };
    Ok(())
}
fn validate_field(k: &str, v: &Value) -> MoteResult<()> {
    match (k, v) {
        ("title", Value::String(s)) if !s.is_empty() => text(s, 4096, "title"),
        ("body", Value::String(s)) => text(s, 1 << 20, "body"),
        ("priority", Value::Number(n)) if n.as_i64().is_some_and(|x| (0..=3).contains(&x)) => {
            Ok(())
        }
        ("assignee", Value::String(s)) => nonblank(s, 128, "assignee"),
        _ => bad("invalid patch field"),
    }
}
impl CoordinationState {
    pub fn view(&self) -> MoteResult<Value> {
        Ok(
            json!({"tasks":replay_tasks(self)?,"sessions":self.sessions,"claims":self.claims,"reservations":self.reservations}),
        )
    }
    pub fn task_view(&self) -> MoteResult<Value> {
        Ok(json!(replay_tasks(self)?))
    }
}

fn replay_tasks(state: &CoordinationState) -> MoteResult<BTreeMap<String, Task>> {
    let entries = state
        .task_entries
        .iter()
        .map(|v| {
            let op = v
                .get("op")
                .and_then(Value::as_str)
                .ok_or_else(|| MoteError::Invalid("invalid coordination task entry".into()))?;
            match serde_json::from_value::<crate::op::Op>(v.clone())? {
                crate::op::Op::Create(_)
                | crate::op::Op::Patch(_)
                | crate::op::Op::TagAdd(_)
                | crate::op::Op::TagRemove(_)
                | crate::op::Op::DepAdd(_)
                | crate::op::Op::DepRemove(_)
                | crate::op::Op::RelAdd(_)
                | crate::op::Op::RelRemove(_)
                | crate::op::Op::Note(_) => (),
                _ => {
                    return Err(MoteError::Invalid(
                        "unsupported coordination task operation".into(),
                    ));
                }
            }
            Ok((format!("{op}.json"), serde_json::to_vec(v)?))
        })
        .collect::<MoteResult<Vec<_>>>()?;
    let r = crate::reducer::replay(entries);
    if state
        .task_entries
        .iter()
        .any(|v| !r.was_accepted(v.get("op").and_then(Value::as_str).unwrap_or("")))
    {
        return Err(MoteError::Invalid(
            "coordination task operation rejected by reducer".into(),
        ));
    }
    if !r.reservations.is_empty()
        || !r.sessions.is_empty()
        || !r.messages.is_empty()
        || !r.roles.is_empty()
        || !r.candidates.is_empty()
        || !r.handoff_orphans.is_empty()
    {
        return Err(MoteError::Invalid(
            "coordination task history used unsupported legacy state".into(),
        ));
    }
    Ok(r.beads
        .into_iter()
        .filter(|(_, b)| !b.is_deleted())
        .map(|(id, b)| {
            (
                id,
                Task {
                    title: b.title,
                    body: b.body,
                    priority: b.priority,
                    assignee: b.assignee,
                    status: b.status.as_str().into(),
                    clock: b.clock,
                    tags: b.tags,
                    deps: b.deps.into_iter().map(|(p, _)| p).collect(),
                    rels: b.rels,
                    notes: b
                        .notes
                        .into_iter()
                        .map(|n| TaskNote {
                            op_id: n.op_id,
                            note_kind: n.note_kind,
                            actor: n.actor,
                            ts: n.ts,
                            text: n.text,
                        })
                        .collect(),
                },
            )
        })
        .collect())
}

pub fn plan(
    state: &CoordinationState,
    request: &Request,
    sequence: u64,
) -> MoteResult<CoordinationPlan> {
    request.validate()?;
    // The task projection is derived once for preconditions. Only the reducer
    // applies task mutations, after the generated allowlisted op is appended.
    let tasks = replay_tasks(state)?;
    let action = match &request.command {
        protocol::Command::Coordination { action } => action,
        _ => return bad("inactive command profile"),
    };
    if action.is_lifecycle() {
        return lifecycle::plan(state, request, sequence);
    }
    let mut next = state.clone();
    let eid = format!("eff-{}-000000", request.request_id);
    let reject = |code: &str, subject: &str| CoordinationPlan {
        effects: vec![],
        outcome: Outcome {
            status: OutcomeStatus::Rejected,
            result: json!({"code":code,"subject":subject}),
        },
        next_state: state.clone(),
    };
    let active = |s: &CoordinationState| {
        s.sessions
            .get(&request.session)
            .is_some_and(|x| x.status == "active" && x.actor == request.actor)
    };
    if !matches!(action, Action::SessionStart { .. }) && !active(state) {
        return Ok(reject("session_unavailable", &request.session));
    }
    let (effect, subject) = match action {
        Action::Begin(_)
        | Action::Next(_)
        | Action::Done(_)
        | Action::Handoff(_)
        | Action::SessionEnd(_)
        | Action::SessionRecover(_) => {
            unreachable!("lifecycle action dispatched before core planning")
        }
        Action::SessionStart { label } => {
            if state.sessions.contains_key(&request.session) {
                return Ok(reject("session_exists", &request.session));
            };
            next.sessions.insert(
                request.session.clone(),
                Session {
                    actor: request.actor.clone(),
                    token: eid.clone(),
                    label: label.clone(),
                    status: "active".into(),
                    presence_timestamp: request.timestamp.clone(),
                    presence_text: String::new(),
                },
            );
            (
                Effect::Ownership {
                    effect_id: eid.clone(),
                    sequence,
                    index: 0,
                    transition: Transition::SessionStarted {
                        session: request.session.clone(),
                        actor: request.actor.clone(),
                        token: eid.clone(),
                        label: label.clone(),
                        timestamp: request.timestamp.clone(),
                    }
                    .into(),
                },
                request.session.clone(),
            )
        }
        Action::SessionPresence { text } => {
            let s = next.sessions.get_mut(&request.session).unwrap();
            s.presence_timestamp = request.timestamp.clone();
            s.presence_text = text.clone();
            (
                Effect::Ownership {
                    effect_id: eid.clone(),
                    sequence,
                    index: 0,
                    transition: Transition::SessionPresence {
                        session: request.session.clone(),
                        actor: request.actor.clone(),
                        text: text.clone(),
                        timestamp: request.timestamp.clone(),
                    }
                    .into(),
                },
                request.session.clone(),
            )
        }
        Action::TaskCreate {
            id,
            title,
            body,
            priority,
            assignee,
        } => {
            if tasks.contains_key(id) {
                return Ok(reject("task_exists", id));
            };
            (
                task_effect(
                    &eid,
                    sequence,
                    request,
                    json!({"kind":"create","entity":id,"set":{"title":title,"body":body,"priority":priority,"assignee":assignee,"status":"open"}}),
                ),
                id.clone(),
            )
        }
        Action::TaskPatch { id, set, expect } => {
            let Some(t) = tasks.get(id) else {
                return Ok(reject("task_unavailable", id));
            };
            for k in set.keys() {
                if t.clock.get(k).cloned() != expect[k].clone() {
                    return Ok(reject("stale_field", k));
                }
            }
            (
                task_effect(
                    &eid,
                    sequence,
                    request,
                    json!({"kind":"patch","entity":id,"set":set,"expect":expect}),
                ),
                id.clone(),
            )
        }
        Action::TaskTag { id, tag, add } => {
            if !tasks.contains_key(id) {
                return Ok(reject("task_unavailable", id));
            };
            (
                task_effect(
                    &eid,
                    sequence,
                    request,
                    json!({"kind":if *add{"tag_add"}else{"tag_remove"},"entity":id,"tag":tag}),
                ),
                id.clone(),
            )
        }
        Action::TaskNote {
            id,
            note_kind,
            text,
        } => {
            if !tasks.contains_key(id) {
                return Ok(reject("task_unavailable", id));
            };
            (
                task_effect(
                    &eid,
                    sequence,
                    request,
                    json!({"kind":"note","entity":id,"note_kind":note_kind,"text":text}),
                ),
                id.clone(),
            )
        }
        Action::ClaimAcquire { id } => {
            let Some(task) = tasks.get(id) else {
                return Ok(reject("task_unavailable", id));
            };
            if !eligible(task) {
                return Ok(reject("task_unavailable", id));
            }
            if state.claims.contains_key(id) {
                return Ok(reject("claim_occupied", id));
            }
            next.claims.insert(
                id.clone(),
                Claim {
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: eid.clone(),
                },
            );
            (
                Effect::Ownership {
                    effect_id: eid.clone(),
                    sequence,
                    index: 0,
                    transition: Transition::ClaimAcquired {
                        task: id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: eid.clone(),
                    }
                    .into(),
                },
                id.clone(),
            )
        }
        Action::ClaimRelease { id, token } => {
            match state.claims.get(id) {
                Some(c)
                    if c.actor == request.actor
                        && c.session == request.session
                        && c.token == *token => {}
                _ => return Ok(reject("claim_mismatch", id)),
            }
            next.claims.remove(id);
            (
                Effect::Ownership {
                    effect_id: eid.clone(),
                    sequence,
                    index: 0,
                    transition: Transition::ClaimReleased {
                        task: id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                    }
                    .into(),
                },
                id.clone(),
            )
        }
        Action::ReservationAcquire {
            id,
            reservation,
            paths,
        } => {
            let Some(task) = tasks.get(id) else {
                return Ok(reject("task_unavailable", id));
            };
            if !eligible(task) {
                return Ok(reject("task_unavailable", id));
            }
            if state.reservations.contains_key(reservation) {
                return Ok(reject("reservation_exists", reservation));
            }
            if let Some((rid, _)) = state
                .reservations
                .iter()
                .find(|(_, r)| r.status == "open" && overlap(&r.paths, paths))
            {
                return Ok(reject("path_conflict", rid));
            }
            next.reservations.insert(
                reservation.clone(),
                Reservation {
                    task: id.clone(),
                    actor: request.actor.clone(),
                    session: request.session.clone(),
                    token: eid.clone(),
                    paths: paths.clone(),
                    status: "open".into(),
                },
            );
            (
                Effect::Ownership {
                    effect_id: eid.clone(),
                    sequence,
                    index: 0,
                    transition: Transition::ReservationAcquired {
                        reservation: reservation.clone(),
                        task: id.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: eid.clone(),
                        paths: paths.clone(),
                    }
                    .into(),
                },
                reservation.clone(),
            )
        }
        Action::ReservationRelease { reservation, token } => {
            match state.reservations.get(reservation) {
                Some(r)
                    if r.status == "open"
                        && r.actor == request.actor
                        && r.session == request.session
                        && r.token == *token => {}
                _ => return Ok(reject("reservation_mismatch", reservation)),
            }
            let r = next.reservations.get_mut(reservation).unwrap();
            r.status = "closed".into();
            (
                Effect::Ownership {
                    effect_id: eid.clone(),
                    sequence,
                    index: 0,
                    transition: Transition::ReservationReleased {
                        reservation: reservation.clone(),
                        task: r.task.clone(),
                        actor: request.actor.clone(),
                        session: request.session.clone(),
                        token: token.clone(),
                    }
                    .into(),
                },
                reservation.clone(),
            )
        }
        Action::TaskDependency { id, parent, add } => {
            let Some(_task) = tasks.get(id) else {
                return Ok(reject("task_unavailable", id));
            };
            let Some(_) = tasks.get(parent) else {
                return Ok(reject("task_unavailable", parent));
            };
            if id == parent {
                return Ok(reject("self_edge", id));
            };
            if *add
                && (state.claims.contains_key(id)
                    || state
                        .reservations
                        .values()
                        .any(|r| r.task == *id && r.status == "open"))
            {
                return Ok(reject("held_work", id));
            };
            if *add && reaches(&tasks, parent, id, false) {
                return Ok(reject("cycle", id));
            };
            (
                task_effect(
                    &eid,
                    sequence,
                    request,
                    json!({"kind":if *add{"dep_add"}else{"dep_remove"},"entity":id,"parent":parent,"dep_kind":"blocks"}),
                ),
                id.clone(),
            )
        }
        Action::TaskRelation {
            id,
            parent,
            relation,
            add,
        } => {
            if !tasks.contains_key(id) {
                return Ok(reject("task_unavailable", id));
            };
            if !tasks.contains_key(parent) {
                return Ok(reject("task_unavailable", parent));
            };
            if id == parent {
                return Ok(reject("self_edge", id));
            };
            if *add && relation == "parent" && reaches(&tasks, parent, id, true) {
                return Ok(reject("cycle", id));
            };
            (
                task_effect(
                    &eid,
                    sequence,
                    request,
                    json!({"kind":if *add{"rel_add"}else{"rel_remove"},"entity":id,"parent":parent,"rel_kind":relation}),
                ),
                id.clone(),
            )
        }
    };
    if let Effect::TaskOp { op, .. } = &effect {
        next.task_entries.push(op.clone());
        if replay_tasks(&next).is_err() {
            return Ok(reject("task_effect_rejected", &subject));
        }
    }
    Ok(CoordinationPlan {
        effects: vec![effect],
        outcome: Outcome {
            status: OutcomeStatus::Accepted,
            result: json!({"code":"ok","subject":subject,"effect_ids":[eid]}),
        },
        next_state: next,
    })
}
fn eligible(task: &Task) -> bool {
    matches!(
        task.status.as_str(),
        "open" | "doing" | "blocked" | "review"
    )
}
fn overlap(a: &[String], b: &[String]) -> bool {
    a.iter()
        .any(|x| b.iter().any(|y| crate::paths::overlap(x, y)))
}
fn reaches(tasks: &BTreeMap<String, Task>, start: &str, target: &str, relations: bool) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![start];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if id == target {
            return true;
        }
        if let Some(t) = tasks.get(id) {
            if relations {
                stack.extend(
                    t.rels
                        .iter()
                        .filter(|(_, k)| k == "parent")
                        .map(|(p, _)| p.as_str()),
                );
            } else {
                stack.extend(t.deps.iter().map(String::as_str));
            }
        }
    }
    false
}
fn task_effect(e: &str, sequence: u64, r: &Request, body: Value) -> Effect {
    task_effect_at(e, sequence, 0, r, body)
}

fn task_effect_at(e: &str, sequence: u64, index: u32, r: &Request, body: Value) -> Effect {
    let mut op = body;
    let o = op.as_object_mut().unwrap();
    if let Some(set) = o.get_mut("set").and_then(Value::as_object_mut) {
        if set.get("assignee").is_some_and(Value::is_null) {
            set.remove("assignee");
        }
    }
    // Remote CAS already verified absent clocks against the parent state.
    // Legacy PatchOp accepts string clocks only, so null expectations remain
    // in the canonical request but are omitted from its reducer adapter.
    if let Some(expect) = o.get_mut("expect").and_then(Value::as_object_mut) {
        expect.retain(|_, value| !value.is_null());
        if expect.is_empty() {
            o.remove("expect");
        }
    }
    o.insert("v".into(), json!(1));
    o.insert(
        "op".into(),
        json!(format!("gaop-{sequence:020}-{index:06}-{e}")),
    );
    o.insert("actor".into(), json!(r.actor));
    o.insert("ts".into(), json!(r.timestamp));
    Effect::TaskOp {
        effect_id: e.into(),
        sequence,
        index,
        op,
    }
}
