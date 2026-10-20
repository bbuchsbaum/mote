//! Strict, reducer-backed discussion and message actions for Git authority.
use super::{CoordinationPlan, CoordinationState};
use crate::git_authority::protocol::{Effect, Outcome, OutcomeStatus, Request};
use crate::{MoteError, MoteResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const BODY_MAX: usize = 1 << 20;
const OPAQUE_MAX: usize = 268_435_584;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reference {
    Topic { topic: String },
    Post { post_id: String },
    Issue { issue_id: String },
    Candidate { candidate_id: String },
    Url { url: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuestionDraft {
    pub question_id: String,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitialPost {
    pub id: String,
    pub body: String,
    pub notify: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub idempotency_key: Option<String>,
}
fn required_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    #[serde(rename = "message.send")]
    MessageSend {
        id: String,
        to: String,
        #[serde(deserialize_with = "required_option")]
        issue: Option<String>,
        #[serde(deserialize_with = "required_option")]
        reservation: Option<String>,
        msg_kind: String,
        body: String,
        answers: Vec<String>,
        #[serde(deserialize_with = "required_option")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "message.reply")]
    MessageReply {
        id: String,
        request_id: String,
        msg_kind: String,
        body: String,
        #[serde(deserialize_with = "required_option")]
        idempotency_key: Option<String>,
    },
    #[serde(rename = "message.ack")]
    MessageAck {
        id: String,
    },
    #[serde(rename = "message.resolve")]
    MessageResolve {
        id: String,
    },
    Post {
        id: String,
        topic: String,
        body: String,
        #[serde(deserialize_with = "required_option")]
        reply_to: Option<String>,
        post_kind: String,
        answers: Vec<String>,
        notify: Vec<String>,
        #[serde(deserialize_with = "required_option")]
        idempotency_key: Option<String>,
    },
    Read {
        #[serde(deserialize_with = "required_option")]
        topic: Option<String>,
        through_post: String,
        #[serde(deserialize_with = "required_option")]
        expect: Option<String>,
    },
    Watch {
        topic: String,
        watching: bool,
    },
    Topic {
        topic: String,
        #[serde(deserialize_with = "required_option")]
        title: Option<String>,
        #[serde(deserialize_with = "required_option")]
        description: Option<String>,
        #[serde(deserialize_with = "required_option")]
        post: Option<InitialPost>,
    },
    Sticky {
        post_id: String,
        sticky: bool,
    },
    Supersede {
        old_post_id: String,
        new_post_id: String,
    },
    Retract {
        post_id: String,
        reason: String,
    },
    Route {
        #[serde(deserialize_with = "required_option")]
        post_id: Option<String>,
        #[serde(deserialize_with = "required_option")]
        topic: Option<String>,
        route_state: String,
        #[serde(deserialize_with = "required_option")]
        issue: Option<String>,
        #[serde(deserialize_with = "required_option")]
        note: Option<String>,
    },
    Decision {
        id: String,
        topic: String,
        body: String,
        agreed_post_ids: Vec<String>,
        references: Vec<Reference>,
        open_questions: Vec<QuestionDraft>,
        notify: Vec<String>,
        #[serde(deserialize_with = "required_option")]
        idempotency_key: Option<String>,
        issues: Vec<String>,
    },
    Question {
        question_id: String,
        action: String,
        expect_question: String,
        references: Vec<Reference>,
        #[serde(deserialize_with = "required_option")]
        note: Option<String>,
        #[serde(deserialize_with = "required_option")]
        successor_question_id: Option<String>,
        #[serde(deserialize_with = "required_option")]
        idempotency_key: Option<String>,
    },
    Promote {
        post_id: String,
        task_id: String,
        title: String,
        body: String,
        priority: i32,
        tags: Vec<String>,
        deps: Vec<String>,
    },
}
fn bad<T>(s: &str) -> MoteResult<T> {
    Err(MoteError::Invalid(s.into()))
}
fn valid_id(s: &str, p: &str) -> MoteResult<()> {
    if !s.starts_with(p) || !crate::git_authority::protocol::simple_identifier(&s[p.len()..], 100) {
        bad("invalid communication ID")
    } else {
        Ok(())
    }
}
fn opaque(s: &str) -> bool {
    !s.is_empty() && s.len() <= OPAQUE_MAX && !s.chars().any(char::is_control)
}
fn text(s: &str, n: usize, blank: bool) -> MoteResult<()> {
    if s.len() > n || (blank && s.trim().is_empty()) {
        bad("invalid communication text")
    } else {
        Ok(())
    }
}
fn sorted<T: Ord>(v: &[T]) -> bool {
    v.windows(2).all(|w| w[0] < w[1])
}
fn topic(s: &str) -> MoteResult<()> {
    if s.is_empty() || s.trim() != s || s.len() > 4096 || s.chars().any(char::is_control) {
        bad("invalid topic")
    } else {
        Ok(())
    }
}
fn reference_id(value: &str) -> MoteResult<()> {
    if opaque(value) {
        Ok(())
    } else {
        bad("invalid communication reference")
    }
}
fn actor(value: &str) -> MoteResult<()> {
    text(value, 128, true)?;
    if value.chars().any(char::is_control) {
        return bad("invalid recipient");
    }
    Ok(())
}
fn list<T: Ord>(values: &[T], maximum: usize) -> MoteResult<()> {
    if values.len() > maximum || !sorted(values) {
        return bad("invalid communication list");
    }
    Ok(())
}
fn identifiers(values: &[String]) -> MoteResult<()> {
    list(values, 64)?;
    for value in values {
        reference_id(value)?;
    }
    Ok(())
}
fn recipients(values: &[String]) -> MoteResult<()> {
    list(values, 64)?;
    for value in values {
        actor(value)?;
    }
    Ok(())
}
fn references(values: &[Reference]) -> MoteResult<()> {
    list(values, 256)?;
    for value in values {
        match value {
            Reference::Topic { topic: name } => topic(name)?,
            Reference::Issue { issue_id } => valid_id(issue_id, "bd-")?,
            Reference::Post { post_id } => reference_id(post_id)?,
            Reference::Candidate { candidate_id } => reference_id(candidate_id)?,
            Reference::Url { url } => reference_id(url)?,
        }
    }
    Ok(())
}

pub fn validate(a: &Action) -> MoteResult<()> {
    match a {
        Action::MessageSend {
            id,
            to,
            issue,
            reservation,
            msg_kind,
            body,
            answers,
            idempotency_key,
        } => {
            valid_id(id, "msg-")?;
            actor(to)?;
            text(body, BODY_MAX, true)?;
            if !matches!(
                msg_kind.as_str(),
                "note" | "request" | "handoff" | "blocked" | "fyi"
            ) {
                return bad("invalid message kind");
            }
            identifiers(answers)?;
            if let Some(id) = issue {
                valid_id(id, "bd-")?;
            }
            if let Some(id) = reservation {
                valid_id(id, "rv-")?;
            }
            key(idempotency_key)?;
        }
        Action::MessageReply {
            id,
            request_id,
            msg_kind,
            body,
            idempotency_key,
        } => {
            valid_id(id, "msg-")?;
            reference_id(request_id)?;
            text(body, BODY_MAX, true)?;
            if !matches!(msg_kind.as_str(), "response" | "decline") {
                return bad("invalid reply kind");
            }
            key(idempotency_key)?;
        }
        Action::MessageAck { id } | Action::MessageResolve { id } => reference_id(id)?,
        Action::Post {
            id,
            topic: name,
            body,
            reply_to,
            post_kind,
            answers,
            notify,
            idempotency_key,
        } => {
            valid_id(id, "post-")?;
            topic(name)?;
            text(body, BODY_MAX, true)?;
            if !matches!(post_kind.as_str(), "post" | "decision" | "summary") {
                return bad("invalid post kind");
            }
            if let Some(id) = reply_to {
                reference_id(id)?;
            }
            identifiers(answers)?;
            recipients(notify)?;
            key(idempotency_key)?;
        }
        Action::Read {
            topic: name,
            through_post,
            expect,
        } => {
            if let Some(name) = name {
                topic(name)?;
            }
            reference_id(through_post)?;
            if let Some(expect) = expect {
                reference_id(expect)?;
            }
        }
        Action::Watch { topic: name, .. } => topic(name)?,
        Action::Topic {
            topic: name,
            title,
            description,
            post,
        } => {
            topic(name)?;
            if let Some(title) = title {
                text(title, 4096, true)?;
            }
            if let Some(body) = description {
                text(body, 4096, false)?;
            }
            if let Some(post) = post {
                valid_id(&post.id, "post-")?;
                text(&post.body, BODY_MAX, true)?;
                recipients(&post.notify)?;
                key(&post.idempotency_key)?;
            }
        }
        Action::Sticky { post_id, .. } => reference_id(post_id)?,
        Action::Supersede {
            old_post_id,
            new_post_id,
        } => {
            reference_id(old_post_id)?;
            reference_id(new_post_id)?;
        }
        Action::Retract { post_id, reason } => {
            reference_id(post_id)?;
            text(reason, 4096, true)?;
            if reason.chars().any(char::is_control) {
                return bad("invalid retraction reason");
            }
        }
        Action::Route {
            post_id,
            topic: name,
            route_state,
            issue,
            note,
        } => {
            if post_id.is_some() == name.is_some()
                || !matches!(
                    route_state.as_str(),
                    "open" | "needs_bead" | "routed" | "resolved"
                )
                || (route_state == "routed") != issue.is_some()
            {
                return bad("invalid route");
            }
            if let Some(id) = post_id {
                reference_id(id)?;
            }
            if let Some(name) = name {
                topic(name)?;
            }
            if let Some(id) = issue {
                valid_id(id, "bd-")?;
            }
            if let Some(note) = note {
                text(note, BODY_MAX, false)?;
            }
        }
        Action::Decision {
            id,
            topic: name,
            body,
            agreed_post_ids,
            references: refs,
            open_questions,
            notify,
            idempotency_key,
            issues,
        } => {
            valid_id(id, "post-")?;
            topic(name)?;
            text(body, BODY_MAX, true)?;
            identifiers(agreed_post_ids)?;
            references(refs)?;
            recipients(notify)?;
            list(issues, 64)?;
            let targets: Vec<_> = refs
                .iter()
                .filter_map(|r| {
                    if let Reference::Issue { issue_id } = r {
                        Some(issue_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if targets != *issues {
                return bad("decision issue targets differ");
            }
            if open_questions.len() > 64 {
                return bad("too many decision questions");
            }
            let mut seen = std::collections::BTreeSet::new();
            for question in open_questions {
                valid_id(&question.question_id, "question-")?;
                text(&question.text, BODY_MAX, true)?;
                if !seen.insert(&question.question_id) {
                    return bad("duplicate decision question");
                }
            }
            key(idempotency_key)?;
        }
        Action::Question {
            question_id,
            action,
            expect_question,
            references: refs,
            note,
            successor_question_id,
            idempotency_key,
        } => {
            reference_id(question_id)?;
            reference_id(expect_question)?;
            references(refs)?;
            if !matches!(action.as_str(), "answer" | "defer" | "supersede" | "close") {
                return bad("invalid question action");
            }
            if let Some(note) = note {
                text(note, BODY_MAX, false)?;
            }
            if let Some(id) = successor_question_id {
                reference_id(id)?;
            }
            key(idempotency_key)?;
        }
        Action::Promote {
            post_id,
            task_id,
            title,
            body,
            priority,
            tags,
            deps,
        } => {
            reference_id(post_id)?;
            valid_id(task_id, "bd-")?;
            text(title, 4096, true)?;
            text(body, BODY_MAX, false)?;
            if !(0..=3).contains(priority) {
                return bad("invalid promotion priority");
            }
            list(tags, 64)?;
            list(deps, 64)?;
            for tag in tags {
                super::nonblank(tag, 128, "tag")?;
            }
            for id in deps {
                valid_id(id, "bd-")?;
            }
        }
    }
    Ok(())
}

fn key(v: &Option<String>) -> MoteResult<()> {
    if v.as_ref().is_some_and(|x| {
        x.trim() != x || x.is_empty() || x.len() > 128 || x.chars().any(char::is_control)
    }) {
        bad("invalid idempotency key")
    } else {
        Ok(())
    }
}

pub(super) fn plan(
    state: &CoordinationState,
    request: &Request,
    sequence: u64,
    action: &Action,
) -> MoteResult<CoordinationPlan> {
    plan_at(state, request, sequence, action, 0)
}

pub(super) fn plan_at(
    state: &CoordinationState,
    request: &Request,
    sequence: u64,
    action: &Action,
    offset: usize,
) -> MoteResult<CoordinationPlan> {
    let domain = state.communication_state()?;
    let reject =
        |code: &str, subject: &str| rejected(state, json!({"code":code,"subject":subject}));
    match action {
        Action::Read {
            topic,
            through_post,
            expect,
        } => {
            if domain.discussion_cursor_for(&request.actor, topic.as_deref()) != expect.as_ref() {
                return Ok(reject("stale_cursor", through_post));
            }
            if !domain.board_posts.contains_key(through_post) {
                return Ok(reject("post_unavailable", through_post));
            }
        }
        Action::MessageReply { request_id, .. } if !domain.messages.contains_key(request_id) => {
            return Ok(reject("message_unavailable", request_id));
        }
        Action::Promote {
            task_id,
            deps,
            post_id,
            ..
        } => {
            if domain.beads.contains_key(task_id) || state.consumed_task(task_id) {
                return Ok(reject("task_exists", task_id));
            }
            for id in deps {
                if domain.beads.get(id).is_none_or(|task| task.is_deleted()) {
                    return Ok(reject("task_unavailable", id));
                }
            }
            if !domain.board_posts.contains_key(post_id) {
                return Ok(reject("post_unavailable", post_id));
            }
        }
        _ => (),
    }
    apply_specs(
        state,
        request,
        sequence,
        &subject(action),
        ops(action, &domain)?,
        domain,
        offset,
    )
}
fn rejected(state: &CoordinationState, result: Value) -> CoordinationPlan {
    CoordinationPlan {
        effects: vec![],
        outcome: Outcome {
            status: OutcomeStatus::Rejected,
            result,
        },
        next_state: state.clone(),
    }
}
fn apply_specs(
    state: &CoordinationState,
    request: &Request,
    sequence: u64,
    subject: &str,
    specs: Vec<Value>,
    mut domain: crate::state::State,
    offset: usize,
) -> MoteResult<CoordinationPlan> {
    if offset
        .checked_add(specs.len())
        .is_none_or(|end| end > 1_000_000)
    {
        return bad("communication effect index overflow");
    }
    let mut effects = Vec::new();
    for (index, mut op) in specs.into_iter().enumerate() {
        let index = (offset + index) as u32;
        let effect_id = format!("eff-{}-{index:06}", request.request_id);
        let is_task = matches!(
            op["kind"].as_str(),
            Some("create" | "tag_add" | "dep_add" | "note")
        );
        let mut effect = if is_task {
            super::task_effect_at(&effect_id, sequence, index, request, op)
        } else {
            let adapter = state.communication_adapter_id(sequence, index, &effect_id);
            let fields = op.as_object_mut().ok_or_else(|| {
                MoteError::Invalid("invalid internal communication adapter".into())
            })?;
            fields.insert("v".into(), json!(1));
            fields.insert("op".into(), json!(adapter));
            fields.insert("actor".into(), json!(request.actor));
            fields.insert("ts".into(), json!(request.timestamp));
            Effect::CommunicationOp {
                effect_id,
                sequence,
                index,
                op,
            }
        };
        let (Effect::TaskOp { op, .. } | Effect::CommunicationOp { op, .. }) = &mut effect else {
            unreachable!()
        };
        let typed: crate::op::Op = serde_json::from_value(op.clone())?;
        if is_task && validate_task_payload(&typed).is_err() {
            return Ok(rejected(
                state,
                json!({"code":"communication_payload_invalid","subject":subject,"index":index}),
            ));
        }
        if !is_task
            && !matches!(
                typed,
                crate::op::Op::MsgSend(_)
                    | crate::op::Op::MsgAck(_)
                    | crate::op::Op::MsgResolve(_)
                    | crate::op::Op::BoardPost(_)
                    | crate::op::Op::BoardDecision(_)
                    | crate::op::Op::BoardQuestion(_)
                    | crate::op::Op::BoardRead(_)
                    | crate::op::Op::BoardWatch(_)
                    | crate::op::Op::BoardTopic(_)
                    | crate::op::Op::BoardSticky(_)
                    | crate::op::Op::BoardSupersede(_)
                    | crate::op::Op::BoardRetract(_)
                    | crate::op::Op::BoardRoute(_)
            )
        {
            return bad("unsupported internal communication adapter");
        }
        // Use the typed local serializer for omissions, never retain a second
        // subtly different JSON spelling of the adapter.
        *op = serde_json::to_value(&typed)?;
        let id = typed.op_id();
        crate::reducer::apply_entry(&mut domain, &format!("{id}.json"), &serde_json::to_vec(op)?);
        if !domain.was_accepted(id) {
            let reason = domain
                .rejection_reason(id)
                .unwrap_or_else(|| "communication reducer rejected operation".into());
            return Ok(rejected(
                state,
                json!({"code":"communication_effect_rejected","subject":subject,"index":index,"reason":reason}),
            ));
        }
        effects.push(effect);
    }
    let mut next_state = state.clone();
    next_state
        .task_entries
        .extend(effects.iter().map(|effect| match effect {
            Effect::TaskOp { op, .. } | Effect::CommunicationOp { op, .. } => op.clone(),
            _ => unreachable!(),
        }));
    Ok(CoordinationPlan {
        outcome: Outcome {
            status: OutcomeStatus::Accepted,
            result: json!({"code":"ok","subject":subject,"effect_ids":effects.iter().map(Effect::effect_id).collect::<Vec<_>>()}),
        },
        effects,
        next_state,
    })
}
fn validate_task_payload(op: &crate::op::Op) -> MoteResult<()> {
    use super::Action as Core;
    let action = match op {
        crate::op::Op::Create(op) => Core::TaskCreate {
            id: op.entity.clone(),
            title: op.set.title.clone().unwrap_or_default(),
            body: op.set.body.clone().unwrap_or_default(),
            priority: op.set.priority.unwrap_or(2),
            assignee: op.set.assignee.clone(),
        },
        crate::op::Op::TagAdd(op) => Core::TaskTag {
            id: op.entity.clone(),
            tag: op.tag.clone(),
            add: true,
        },
        crate::op::Op::DepAdd(op) => Core::TaskDependency {
            id: op.entity.clone(),
            parent: op.parent.clone(),
            add: true,
        },
        crate::op::Op::Note(op) => Core::TaskNote {
            id: op.entity.clone(),
            note_kind: op.note_kind.clone(),
            text: op.text.clone(),
        },
        _ => return bad("unsupported communication task adapter"),
    };
    super::validate_action(&action)
}

fn subject(a: &Action) -> String {
    match a {
        Action::MessageSend { id, .. }
        | Action::MessageReply { id, .. }
        | Action::MessageAck { id }
        | Action::MessageResolve { id }
        | Action::Post { id, .. }
        | Action::Decision { id, .. } => id.clone(),
        Action::Read { through_post, .. } => through_post.clone(),
        Action::Watch { topic, .. } | Action::Topic { topic, .. } => topic.clone(),
        Action::Sticky { post_id, .. } | Action::Retract { post_id, .. } => post_id.clone(),
        Action::Promote { task_id, .. } => task_id.clone(),
        Action::Supersede { old_post_id, .. } => old_post_id.clone(),
        Action::Route { post_id, topic, .. } => post_id
            .clone()
            .unwrap_or_else(|| format!("topic {}", topic.as_ref().unwrap())),
        Action::Question { question_id, .. } => question_id.clone(),
    }
}
fn ops(a: &Action, domain: &crate::state::State) -> MoteResult<Vec<Value>> {
    let one = |v: Value| Ok(vec![v]);
    match a {
        Action::MessageSend {
            id,
            to,
            issue,
            reservation,
            msg_kind,
            body,
            answers,
            idempotency_key,
        } => one(
            json!({"kind":"msg_send","msg_id":id,"to":to,"entity":issue,"reservation":reservation,"msg_kind":msg_kind,"body":body,"correlation_id":if msg_kind=="request"{Some(id)}else{None::<&String>},"idempotency_key":idempotency_key,"answers":answers,"require_live":false}),
        ),
        Action::MessageReply {
            id,
            request_id,
            msg_kind,
            body,
            idempotency_key,
        } => {
            let root = domain
                .messages
                .get(request_id)
                .ok_or_else(|| MoteError::Invalid("message_unavailable".into()))?;
            one(
                json!({"kind":"msg_send","msg_id":id,"to":root.from,"entity":root.entity,"reservation":root.reservation,"msg_kind":msg_kind,"body":body,"reply_to":request_id,"correlation_id":root.correlation_id,"idempotency_key":idempotency_key,"answers":[],"require_live":false}),
            )
        }
        Action::MessageAck { id } => one(json!({"kind":"msg_ack","msg_id":id})),
        Action::MessageResolve { id } => one(json!({"kind":"msg_resolve","msg_id":id})),
        Action::Post {
            id,
            topic,
            body,
            reply_to,
            post_kind,
            answers,
            notify,
            idempotency_key,
        } => {
            let mut x = vec![
                json!({"kind":"board_post","post_id":id,"topic":topic,"body":body,"reply_to":reply_to,"post_kind":post_kind,"answers":answers,"notify":notify,"idempotency_key":idempotency_key}),
            ];
            if post_kind == "decision" || post_kind == "summary" {
                x.push(json!({"kind":"board_sticky","post_id":id,"sticky":true}));
            }
            Ok(x)
        }
        Action::Read {
            topic,
            through_post,
            ..
        } => {
            let post = &domain.board_posts[through_post];
            one(
                json!({"kind":"board_read","upto_op_id":post.sent_op_id,"topic":topic,"strict":true}),
            )
        }
        Action::Watch { topic, watching } => {
            one(json!({"kind":"board_watch","topic":topic,"watching":watching}))
        }
        Action::Topic {
            topic,
            title,
            description,
            post,
        } => {
            let mut x =
                vec![json!({"kind":"board_topic","topic":topic,"title":title,"body":description})];
            if let Some(p) = post {
                x.push(json!({"kind":"board_post","post_id":p.id,"topic":topic,"body":p.body,"post_kind":"post","notify":p.notify,"idempotency_key":p.idempotency_key}))
            }
            Ok(x)
        }
        Action::Sticky { post_id, sticky } => {
            one(json!({"kind":"board_sticky","post_id":post_id,"sticky":sticky}))
        }
        Action::Supersede {
            old_post_id,
            new_post_id,
        } => one(
            json!({"kind":"board_supersede","old_post_id":old_post_id,"new_post_id":new_post_id}),
        ),
        Action::Retract { post_id, reason } => {
            one(json!({"kind":"board_retract","post_id":post_id,"reason":reason}))
        }
        Action::Route {
            post_id,
            topic,
            route_state,
            issue,
            note,
        } => {
            let mut specs = vec![
                json!({"kind":"board_route","post_id":post_id,"topic":topic,"route_state":route_state,"entity":issue,"note":note}),
            ];
            if let Some(issue) = issue {
                let target = post_id.clone().unwrap_or_else(|| {
                    format!("topic {}", topic.as_ref().expect("validated route target"))
                });
                let text = match note {
                    Some(note) => format!("routed from discussion {target}: {note}"),
                    None => format!("routed from discussion {target}"),
                };
                specs
                    .push(json!({"kind":"note","entity":issue,"note_kind":"decision","text":text}));
            }
            Ok(specs)
        }
        Action::Decision {
            id,
            topic,
            body,
            agreed_post_ids,
            references,
            open_questions,
            notify,
            idempotency_key,
            issues,
        } => {
            let mut specs = vec![
                json!({"kind":"board_decision","post_id":id,"topic":topic,"body":body,"agreed_post_ids":agreed_post_ids,"references":references,"open_questions":open_questions,"notify":notify,"idempotency_key":idempotency_key}),
            ];
            for issue in issues {
                specs.push(json!({"kind":"board_route","post_id":id,"route_state":"routed","entity":issue}));
                specs.push(json!({"kind":"note","entity":issue,"note_kind":"decision","text":format!("routed from discussion {id}")}));
            }
            Ok(specs)
        }
        Action::Question {
            question_id,
            action,
            expect_question,
            references,
            note,
            successor_question_id,
            idempotency_key,
        } => one(
            json!({"kind":"board_question","question_id":question_id,"action":action,"expect_question":expect_question,"references":references,"note":note,"successor_question_id":successor_question_id,"idempotency_key":idempotency_key}),
        ),
        Action::Promote {
            post_id,
            task_id,
            title,
            body,
            priority,
            tags,
            deps,
        } => {
            let post = &domain.board_posts[post_id];
            let provenance = format!(
                "promoted from discussion post {post_id} in topic {}",
                post.topic
            );
            let mut specs = vec![
                json!({"kind":"create","entity":task_id,"set":{"title":title,"body":format!("{body}\n\n{provenance}"),"status":"open","priority":priority}}),
            ];
            specs.extend(
                tags.iter()
                    .map(|tag| json!({"kind":"tag_add","entity":task_id,"tag":tag})),
            );
            specs.extend(deps.iter().map(|parent| json!({"kind":"dep_add","entity":task_id,"parent":parent,"dep_kind":"blocks"})));
            specs.push(json!({"kind":"board_route","post_id":post_id,"route_state":"routed","entity":task_id,"note":format!("promoted to {task_id}")}));
            specs.push(
                json!({"kind":"note","entity":task_id,"note_kind":"decision","text":provenance}),
            );
            Ok(specs)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_authority::{
        coordination::Action as Core,
        planner::AuthorityState,
        protocol::{Command, Manifest, Transaction},
    };

    struct Harness {
        state: AuthorityState,
        counter: u64,
    }
    impl Harness {
        fn new() -> Self {
            Self::seeded(CoordinationState::default())
        }
        fn seeded(coordination: CoordinationState) -> Self {
            let mut h = Self {
                state: AuthorityState {
                    coordination,
                    ..AuthorityState::default()
                },
                counter: 0,
            };
            for actor in ["alice", "bob"] {
                let tx = h.core(
                    actor,
                    Core::SessionStart {
                        label: actor.into(),
                    },
                );
                assert_eq!(tx.outcome.status, OutcomeStatus::Accepted);
            }
            h
        }
        fn request(&mut self, actor: &str, action: Core) -> Request {
            self.counter += 1;
            Request {
                version: 1,
                request_id: format!("request{}", self.counter),
                actor: actor.into(),
                session: format!("session-{actor}"),
                timestamp: "2000-01-01T00:00:00Z".into(),
                command: Command::Coordination { action },
            }
        }
        fn core(&mut self, actor: &str, action: Core) -> Transaction {
            let request = self.request(actor, action);
            let parent = "1".repeat(40);
            let tx = Transaction::prepare(&self.state, &parent, request).unwrap();
            self.state = tx.validate(&self.state, &parent).unwrap();
            tx
        }
        fn send(&mut self, actor: &str, action: Action) -> Transaction {
            self.core(actor, Core::Communication { action })
        }
        fn task(&mut self, id: &str) {
            assert_eq!(
                self.core(
                    "alice",
                    Core::TaskCreate {
                        id: id.into(),
                        title: id.into(),
                        body: String::new(),
                        priority: 2,
                        assignee: None
                    }
                )
                .outcome
                .status,
                OutcomeStatus::Accepted
            );
        }
        fn post(&mut self, id: &str) -> Transaction {
            self.send(
                "alice",
                Action::Post {
                    id: id.into(),
                    topic: "design".into(),
                    body: "a design".into(),
                    reply_to: None,
                    post_kind: "post".into(),
                    answers: vec![],
                    notify: vec![],
                    idempotency_key: None,
                },
            )
        }
    }
    fn promote(body: String) -> Action {
        Action::Promote {
            post_id: "post-source".into(),
            task_id: "bd-promoted".into(),
            title: "implement".into(),
            body,
            priority: 2,
            tags: vec![],
            deps: vec![],
        }
    }
    fn entry(id: &str, actor: &str, kind: &str, extra: Value) -> (String, Vec<u8>) {
        let mut value =
            json!({"v":1,"op":id,"actor":actor,"kind":kind,"ts":"2026-10-20T00:00:00Z"});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        (format!("{id}.json"), serde_json::to_vec(&value).unwrap())
    }
    // This is a private adapter-engine fixture, not a legacy archive/profile
    // verification bypass available to a production client.
    fn imported() -> (CoordinationState, String) {
        let maximum = "雪".repeat(150);
        let entries = vec![
            entry(
                "0",
                "alice",
                "create",
                json!({"entity":"bd-old","set":{"title":"old task"}}),
            ),
            entry(
                "a",
                "alice",
                "msg_send",
                json!({"msg_id":"msg-雪","to":"bob","entity":"bd-old","msg_kind":"request","body":"legacy request","idempotency_key":"old-key"}),
            ),
            entry(
                "az",
                "alice",
                "board_topic",
                json!({"topic":"design","title":"Design"}),
            ),
            entry(
                "b",
                "bob",
                "board_watch",
                json!({"topic":"design","watching":true}),
            ),
            entry(
                "文",
                "alice",
                "board_post",
                json!({"post_id":"post-文","topic":"design","body":"legacy post"}),
            ),
            entry(
                "文a",
                "bob",
                "board_read",
                json!({"upto_op_id":"文","topic":"design"}),
            ),
            entry(&maximum, "alice", "future_unknown", json!({})),
        ];
        let seed = super::super::LegacySeed {
            entries,
            export_digest: "e".repeat(64),
            consumed_tasks: std::collections::BTreeSet::from(["bd-old".into()]),
            consumed_sessions: Default::default(),
            consumed_reservations: Default::default(),
        };
        (
            CoordinationState {
                legacy_seed: Some(seed),
                ..CoordinationState::default()
            },
            maximum,
        )
    }

    #[test]
    fn profile_gates_are_distinct_and_nested_wire_fields_are_strict() {
        let command = Command::Coordination {
            action: Core::Communication {
                action: Action::MessageAck {
                    id: "msg-test".into(),
                },
            },
        };
        for manifest in [
            Manifest::new("st-test".into()),
            Manifest::coordination("st-test".into()),
            Manifest::lifecycle("st-test".into()),
            Manifest::migrated("st-test".into(), "a".repeat(64)),
            Manifest::migrated_recoverable("st-test".into(), "a".repeat(64)),
        ] {
            assert!(manifest.allows(&command).is_err());
        }
        for manifest in [
            Manifest::communications("st-test".into()),
            Manifest::migrated_communications("st-test".into(), "a".repeat(64)),
        ] {
            manifest.allows(&command).unwrap();
            manifest
                .allows(&Command::Coordination {
                    action: Core::SessionEnd(super::super::lifecycle::SessionEnd {
                        expect_session: "token".into(),
                        reason: "finished".into(),
                    }),
                })
                .unwrap();
        }
        let mut value = serde_json::to_value(Action::Decision {
            id: "post-decision".into(),
            topic: "design".into(),
            body: "agreed".into(),
            agreed_post_ids: vec!["post-one".into()],
            references: vec![Reference::Post {
                post_id: "post-one".into(),
            }],
            open_questions: vec![],
            notify: vec![],
            idempotency_key: None,
            issues: vec![],
        })
        .unwrap();
        value["references"][0]["actor"] = json!("override");
        assert!(serde_json::from_value::<Action>(value).is_err());
        let mut value = serde_json::to_value(Action::Read {
            topic: None,
            through_post: "post-one".into(),
            expect: None,
        })
        .unwrap();
        value.as_object_mut().unwrap().remove("expect");
        assert!(serde_json::from_value::<Action>(value).is_err());
    }

    #[test]
    fn compound_effect_offsets_are_checked_before_applying_any_adapter() {
        let mut h = Harness::new();
        let action = Action::Post {
            id: "post-offset".into(),
            topic: "design".into(),
            body: "offset".into(),
            reply_to: None,
            post_kind: "post".into(),
            answers: vec![],
            notify: vec![],
            idempotency_key: None,
        };
        let request = h.request(
            "alice",
            Core::Communication {
                action: action.clone(),
            },
        );
        let state = &h.state.coordination;
        for offset in [1_000_000, usize::MAX] {
            assert!(plan_at(state, &request, h.state.sequence + 1, &action, offset).is_err());
        }
        let ordinary = plan(state, &request, h.state.sequence + 1, &action).unwrap();
        let zero = plan_at(state, &request, h.state.sequence + 1, &action, 0).unwrap();
        assert_eq!(ordinary, zero);
        let last = plan_at(state, &request, h.state.sequence + 1, &action, 999_999).unwrap();
        assert_eq!(last.effects[0].index(), 999_999);
        assert_eq!(
            last.effects[0].effect_id(),
            format!("eff-{}-999999", request.request_id)
        );
        assert!(last.effects[0].adapter_id().contains("-999999-"));
        assert_eq!(state.communication_state().unwrap().board_posts.len(), 0);
    }
    #[test]
    fn read_conflicts_and_missing_reply_targets_are_canonical_rejections() {
        let mut h = Harness::new();
        h.post("post-source");
        assert_eq!(
            h.send(
                "bob",
                Action::Read {
                    topic: Some("design".into()),
                    through_post: "post-source".into(),
                    expect: None
                }
            )
            .outcome
            .status,
            OutcomeStatus::Accepted
        );
        let before = h.state.coordination.clone();
        let tx = h.send(
            "bob",
            Action::Read {
                topic: Some("design".into()),
                through_post: "post-missing".into(),
                expect: None,
            },
        );
        assert_eq!(
            tx.outcome.result,
            json!({"code":"stale_cursor","subject":"post-missing"})
        );
        assert!(tx.effects.is_empty());
        assert_eq!(h.state.coordination, before);
        let tx = h.send(
            "alice",
            Action::Read {
                topic: None,
                through_post: "post-missing".into(),
                expect: None,
            },
        );
        assert_eq!(
            tx.outcome.result,
            json!({"code":"post_unavailable","subject":"post-missing"})
        );
        let tx = h.send(
            "bob",
            Action::MessageReply {
                id: "msg-response".into(),
                request_id: "msg-missing".into(),
                msg_kind: "response".into(),
                body: "reply".into(),
                idempotency_key: None,
            },
        );
        assert_eq!(
            tx.outcome.result,
            json!({"code":"message_unavailable","subject":"msg-missing"})
        );
    }

    #[test]
    fn generated_payload_overflow_discards_prior_route_and_promotion_create() {
        let mut h = Harness::new();
        h.task("bd-target");
        h.post("post-source");
        let before = h.state.coordination.clone();
        let tx = h.send(
            "alice",
            Action::Route {
                post_id: Some("post-source".into()),
                topic: None,
                route_state: "routed".into(),
                issue: Some("bd-target".into()),
                note: Some("n".repeat(BODY_MAX)),
            },
        );
        assert_eq!(
            tx.outcome.result,
            json!({"code":"communication_payload_invalid","subject":"post-source","index":1})
        );
        assert!(tx.effects.is_empty());
        assert_eq!(h.state.coordination, before);
        let tx = h.send("alice", promote("b".repeat(BODY_MAX)));
        assert_eq!(
            tx.outcome.result,
            json!({"code":"communication_payload_invalid","subject":"bd-promoted","index":0})
        );
        assert!(tx.effects.is_empty());
        assert_eq!(h.state.coordination, before);
    }

    #[test]
    fn private_injected_final_promotion_reducer_failure_discards_every_effect() {
        let mut h = Harness::new();
        h.post("post-source");
        let action = promote("body".into());
        let request = h.request(
            "alice",
            Core::Communication {
                action: action.clone(),
            },
        );
        let domain = h.state.coordination.communication_state().unwrap();
        let mut specs = ops(&action, &domain).unwrap();
        specs.last_mut().unwrap()["entity"] = json!("bd-missing");
        let before = h.state.coordination.clone();
        let plan = apply_specs(
            &before,
            &request,
            h.state.sequence + 1,
            "bd-promoted",
            specs,
            domain,
            0,
        )
        .unwrap();
        assert_eq!(plan.outcome.result["code"], "communication_effect_rejected");
        assert_eq!(plan.outcome.result["index"], 2);
        assert!(plan.effects.is_empty());
        assert_eq!(plan.next_state, before);
        assert!(
            !plan
                .next_state
                .communication_state()
                .unwrap()
                .beads
                .contains_key("bd-promoted")
        );
    }

    #[test]
    fn promotion_effects_mix_task_and_communication_in_global_order() {
        let mut h = Harness::new();
        h.task("bd-parent");
        h.post("post-source");
        let tx = h.send(
            "bob",
            Action::Promote {
                post_id: "post-source".into(),
                task_id: "bd-promoted".into(),
                title: "implement".into(),
                body: "body".into(),
                priority: 1,
                tags: vec!["design".into()],
                deps: vec!["bd-parent".into()],
            },
        );
        assert_eq!(tx.outcome.status, OutcomeStatus::Accepted);
        assert_eq!(tx.outcome.result["subject"], "bd-promoted");
        assert_eq!(tx.effects.len(), 5);
        assert!(matches!(tx.effects[3], Effect::CommunicationOp { .. }));
        for (index, effect) in tx.effects.iter().enumerate() {
            assert_eq!(effect.index(), index as u32);
        }
        let state = h.state.coordination.communication_state().unwrap();
        assert_eq!(
            state.beads["bd-promoted"].body,
            "body\n\npromoted from discussion post post-source in topic design"
        );
        assert!(
            state.board_posts["post-source"]
                .route
                .issues
                .contains("bd-promoted")
        );
        assert_eq!(
            state.beads["bd-promoted"].notes[0].text,
            "promoted from discussion post post-source in topic design"
        );
    }

    #[test]
    fn imported_unicode_cursors_and_rejected_maximum_preserve_global_order() {
        let (seed, maximum) = imported();
        let mut h = Harness::seeded(seed);
        let first = h.post("post-first");
        assert_eq!(first.outcome.status, OutcomeStatus::Accepted);
        let adapter = first.effects[0].adapter_id();
        assert!(adapter.starts_with(&format!("{maximum}~gaop-")));
        assert!(adapter.as_str() > maximum.as_str());
        h.task("bd-new");
        let state = h.state.coordination.communication_state().unwrap();
        assert_eq!(state.unread_board_posts_for("bob", Some("design")).len(), 1);
        assert_eq!(
            state.board_posts["post-first"].notification_recipients,
            vec!["bob"]
        );
        let tx = h.send(
            "bob",
            Action::Read {
                topic: Some("design".into()),
                through_post: "post-first".into(),
                expect: Some("文".into()),
            },
        );
        assert_eq!(tx.outcome.status, OutcomeStatus::Accepted);
        h.post("post-second");
        let state = h.state.coordination.communication_state().unwrap();
        assert_eq!(
            state.unread_board_posts_for("bob", Some("design"))[0].post_id,
            "post-second"
        );
        assert_eq!(
            state.board_posts["post-first"].sent_ts,
            "2000-01-01T00:00:00Z"
        );
        assert!(state.beads.contains_key("bd-new"));
    }

    #[test]
    fn imported_request_reply_context_and_consumed_keys_are_preserved() {
        let (seed, _) = imported();
        let mut h = Harness::seeded(seed);
        let before = h.state.coordination.clone();
        for body in ["legacy request", "changed"] {
            let tx = h.send(
                "alice",
                Action::MessageSend {
                    id: "msg-new".into(),
                    to: "bob".into(),
                    issue: Some("bd-old".into()),
                    reservation: None,
                    msg_kind: "request".into(),
                    body: body.into(),
                    answers: vec![],
                    idempotency_key: Some("old-key".into()),
                },
            );
            assert_eq!(tx.outcome.result["code"], "communication_effect_rejected");
            assert!(
                tx.outcome.result["reason"]
                    .as_str()
                    .unwrap()
                    .contains("old-key")
            );
            assert_eq!(h.state.coordination, before);
        }
        let tx = h.send(
            "bob",
            Action::MessageReply {
                id: "msg-response".into(),
                request_id: "msg-雪".into(),
                msg_kind: "response".into(),
                body: "reviewed".into(),
                idempotency_key: None,
            },
        );
        assert_eq!(tx.outcome.status, OutcomeStatus::Accepted);
        let state = h.state.coordination.communication_state().unwrap();
        assert_eq!(
            state.messages["msg-response"].entity.as_deref(),
            Some("bd-old")
        );
        assert_eq!(
            state.messages["msg-response"].correlation_id.as_deref(),
            Some("msg-雪")
        );
        assert_eq!(
            state.messages["msg-雪"].response_msg_id.as_deref(),
            Some("msg-response")
        );
    }

    #[test]
    fn late_initial_post_key_conflict_does_not_create_its_topic() {
        let mut h = Harness::new();
        let first = h.send(
            "alice",
            Action::Post {
                id: "post-key".into(),
                topic: "design".into(),
                body: "keyed".into(),
                reply_to: None,
                post_kind: "post".into(),
                answers: vec![],
                notify: vec![],
                idempotency_key: Some("board-key".into()),
            },
        );
        assert_eq!(first.outcome.status, OutcomeStatus::Accepted);
        let before = h.state.coordination.clone();
        let tx = h.send(
            "alice",
            Action::Topic {
                topic: "new-topic".into(),
                title: Some("new topic".into()),
                description: None,
                post: Some(InitialPost {
                    id: "post-new".into(),
                    body: "initial".into(),
                    notify: vec![],
                    idempotency_key: Some("board-key".into()),
                }),
            },
        );
        assert_eq!(tx.outcome.result["code"], "communication_effect_rejected");
        assert_eq!(tx.outcome.result["index"], 1);
        assert!(tx.effects.is_empty());
        assert_eq!(h.state.coordination, before);
        assert!(
            !h.state
                .coordination
                .communication_state()
                .unwrap()
                .board_topics
                .contains_key("new-topic")
        );
    }

    #[test]
    fn cited_decision_routes_and_question_transitions_use_actual_parent_clocks() {
        let mut h = Harness::new();
        h.task("bd-target");
        h.post("post-source");
        let tx = h.send(
            "alice",
            Action::Decision {
                id: "post-00000000000000000000000001".into(),
                topic: "design".into(),
                body: "agreed design".into(),
                agreed_post_ids: vec!["post-source".into()],
                references: vec![Reference::Issue {
                    issue_id: "bd-target".into(),
                }],
                open_questions: vec![QuestionDraft {
                    question_id: "question-00000000000000000000000002".into(),
                    text: "Does this work?".into(),
                }],
                notify: vec!["bob".into()],
                idempotency_key: Some("decision-key".into()),
                issues: vec!["bd-target".into()],
            },
        );
        assert_eq!(tx.outcome.status, OutcomeStatus::Accepted);
        assert_eq!(tx.effects.len(), 3);
        let domain = h.state.coordination.communication_state().unwrap();
        assert!(domain.board_posts["post-00000000000000000000000001"].sticky);
        assert_eq!(
            domain.beads["bd-target"].notes[0].text,
            "routed from discussion post-00000000000000000000000001"
        );
        let clock = domain.board_questions["question-00000000000000000000000002"]
            .clock_op_id
            .clone();
        let answer = Action::Question {
            question_id: "question-00000000000000000000000002".into(),
            action: "answer".into(),
            expect_question: clock,
            references: vec![Reference::Post {
                post_id: "post-source".into(),
            }],
            note: Some("candidate answer".into()),
            successor_question_id: None,
            idempotency_key: Some("answer-key".into()),
        };
        assert_eq!(
            h.send("bob", answer.clone()).outcome.status,
            OutcomeStatus::Accepted
        );
        let before = h.state.coordination.clone();
        assert_eq!(
            h.send("bob", answer).outcome.status,
            OutcomeStatus::Rejected
        );
        assert_eq!(h.state.coordination, before);
        let clock = h
            .state
            .coordination
            .communication_state()
            .unwrap()
            .board_questions["question-00000000000000000000000002"]
            .clock_op_id
            .clone();
        let closed = h.send(
            "alice",
            Action::Question {
                question_id: "question-00000000000000000000000002".into(),
                action: "close".into(),
                expect_question: clock,
                references: vec![Reference::Post {
                    post_id: "post-source".into(),
                }],
                note: Some("resolved".into()),
                successor_question_id: None,
                idempotency_key: Some("close-key".into()),
            },
        );
        assert_eq!(closed.outcome.status, OutcomeStatus::Accepted);
        let state = h.state.coordination.communication_state().unwrap();
        assert_eq!(
            state.board_questions["question-00000000000000000000000002"]
                .status
                .as_str(),
            "closed"
        );
        assert!(h.state.coordination.view().unwrap()["communications"]["board_questions"]["question-00000000000000000000000002"].is_object());
    }
}
