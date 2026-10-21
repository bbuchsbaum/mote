//! CLI communication commands share canonical request and observation plumbing.
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::{Value, json};

use super::{open_client, print_value, required_session, submit_action, view_with_data};
use crate::cli::{
    Cli, Command, DiscussCmd, DiscussQuestionCmd, DiscussTopicCmd, MsgCmd,
    normalize_discussion_topic, require_post_text, resolve_decision_body, resolve_optional_text,
    resolve_positional_text,
};
use crate::git_authority::{
    connection,
    coordination::{
        Action as CoordinationAction,
        communications::{Action, InitialPost, QuestionDraft, Reference},
    },
    history::Snapshot,
};
use crate::state::{BoardPostRecord, State};
use crate::{MoteError, MoteResult, Store, ids};

pub(super) fn run(cli: &Cli, store: &Store) -> MoteResult<i32> {
    let descriptor = connection::load_descriptor(store.root())?.ok_or_else(|| {
        MoteError::Invalid("Git setup is incomplete; run authority resume".into())
    })?;
    if !descriptor
        .capabilities
        .iter()
        .any(|c| c == "coordination-communications-v1")
    {
        return Err(MoteError::Invalid(
            "this immutable authority profile does not support communications".into(),
        ));
    }
    match &cli.command {
        Command::Send {
            to,
            issue,
            reservation,
            msg_kind,
            idempotency_key,
            require_live,
            answers,
            text,
            stdin,
        } => message(
            cli,
            store,
            &MsgCmd::Send {
                to: to.clone(),
                issue: issue.clone(),
                reservation: reservation.clone(),
                msg_kind: msg_kind.clone(),
                idempotency_key: idempotency_key.clone(),
                require_live: *require_live,
                answers: answers.clone(),
                text: text.clone(),
                stdin: *stdin,
            },
        ),
        Command::Msg { cmd } => message(cli, store, cmd),
        Command::Discuss { cmd } => discussion(cli, store, cmd),
        Command::Inbox { .. } => inbox(cli, store),
        _ => unreachable!("communication command routing"),
    }
}

fn sorted<T: Ord>(mut values: Vec<T>) -> Vec<T> {
    values.sort();
    values.dedup();
    values
}
fn references(values: &[String]) -> MoteResult<Vec<Reference>> {
    let references = values
        .iter()
        .map(|value| {
            let (kind, target) = value.split_once(':').ok_or_else(|| {
                MoteError::Invalid(
                    "reference requires topic:, post:, issue:, candidate:, or url:".into(),
                )
            })?;
            if target.is_empty() {
                return Err(MoteError::Invalid("reference target is empty".into()));
            }
            Ok(match kind {
                "topic" => Reference::Topic {
                    topic: normalize_discussion_topic(target)?,
                },
                "post" => Reference::Post {
                    post_id: target.into(),
                },
                "issue" => Reference::Issue {
                    issue_id: target.into(),
                },
                "candidate" => Reference::Candidate {
                    candidate_id: target.into(),
                },
                "url" => Reference::Url { url: target.into() },
                _ => return Err(MoteError::Invalid("unknown reference kind".into())),
            })
        })
        .collect::<MoteResult<Vec<Reference>>>()?;
    Ok(sorted(references))
}

fn send(cli: &Cli, store: &Store, action: Action) -> MoteResult<i32> {
    submit_action(
        store,
        cli,
        CoordinationAction::Communication { action },
        required_session()?,
    )
}
fn observe(store: &Store) -> MoteResult<(Snapshot, State)> {
    let (client, _scratch) = open_client(store)?;
    let snapshot = client.refresh()?;
    let state = snapshot.state.coordination.communication_state()?;
    Ok((snapshot, state))
}
fn output(cli: &Cli, snapshot: &Snapshot, data: Value) -> MoteResult<i32> {
    print_value(cli.json, view_with_data(snapshot, data));
    Ok(0)
}

fn message_value(message: &crate::state::MsgRecord) -> Value {
    let mut value = crate::cli::message_json(message);
    value["sent_op_id"] = json!(message.sent_op_id);
    value["ack_op_id"] = json!(message.ack_op_id);
    value
}
fn post_values(state: &State, posts: &[&BoardPostRecord]) -> Value {
    json!(
        posts
            .iter()
            .map(|post| crate::cli::board_post_json_with_state(state, post))
            .collect::<Vec<_>>()
    )
}

fn message(cli: &Cli, store: &Store, cmd: &MsgCmd) -> MoteResult<i32> {
    let action = match cmd {
        MsgCmd::Send {
            to,
            issue,
            reservation,
            msg_kind,
            idempotency_key,
            require_live,
            answers,
            text,
            stdin,
        } => {
            if *require_live {
                return Err(MoteError::Invalid(
                    "Git authority cannot guarantee live delivery; --require-live is unsupported"
                        .into(),
                ));
            }
            Action::MessageSend {
                id: ids::new_msg_id(),
                to: to.clone(),
                issue: issue.clone(),
                reservation: reservation.clone(),
                msg_kind: msg_kind.clone(),
                body: resolve_positional_text(text.clone(), *stdin, "message text")?,
                answers: sorted(answers.clone()),
                idempotency_key: idempotency_key.clone(),
            }
        }
        MsgCmd::Reply {
            msg_id,
            msg_kind,
            idempotency_key,
            text,
            stdin,
        } => Action::MessageReply {
            id: ids::new_msg_id(),
            request_id: msg_id.clone(),
            msg_kind: msg_kind.clone(),
            body: resolve_positional_text(text.clone(), *stdin, "reply text")?,
            idempotency_key: idempotency_key.clone(),
        },
        MsgCmd::Ack { msg_id } => Action::MessageAck { id: msg_id.clone() },
        MsgCmd::Resolve { msg_id } => Action::MessageResolve { id: msg_id.clone() },
        MsgCmd::Thread {
            peer,
            issue,
            msg_kind,
        } => {
            let (snapshot, state) = observe(store)?;
            let actor = store.resolve_actor(cli.actor.as_deref())?;
            let rows: Vec<_> = state
                .conversation_between(&actor, peer)
                .into_iter()
                .filter(|m| {
                    issue
                        .as_ref()
                        .is_none_or(|id| m.entity.as_ref() == Some(id))
                })
                .filter(|m| msg_kind.as_ref().is_none_or(|kind| &m.msg_kind == kind))
                .collect();
            let rows: Vec<_> = rows
                .iter()
                .map(|m| {
                    let mut value = message_value(m);
                    value["direction"] = json!(if m.from == actor { "out" } else { "in" });
                    value
                })
                .collect();
            return output(cli, &snapshot, json!(rows));
        }
        MsgCmd::Requests { request_state } => {
            if request_state.as_ref().is_some_and(|v| {
                !matches!(v.as_str(), "open" | "responded" | "declined" | "resolved")
            }) {
                return Err(MoteError::Invalid("invalid request state".into()));
            }
            let (snapshot, state) = observe(store)?;
            let actor = store.resolve_actor(cli.actor.as_deref())?;
            let rows: Vec<_> = state
                .requests_for(&actor)
                .into_iter()
                .filter(|m| {
                    request_state.as_ref().is_none_or(|wanted| {
                        m.request_state
                            .as_ref()
                            .is_some_and(|actual| actual.as_str() == wanted)
                    })
                })
                .collect();
            return output(
                cli,
                &snapshot,
                json!(rows.iter().map(|m| message_value(m)).collect::<Vec<_>>()),
            );
        }
    };
    send(cli, store, action)
}

fn discussion(cli: &Cli, store: &Store, cmd: &DiscussCmd) -> MoteResult<i32> {
    let action = match cmd {
        DiscussCmd::Post {
            topic,
            reply_to,
            answers,
            notify,
            idempotency_key,
            body,
            text,
        } => Action::Post {
            id: ids::new_post_id(),
            topic: normalize_discussion_topic(topic)?,
            body: require_post_text(text.clone(), body.clone(), "post")?,
            reply_to: reply_to.clone(),
            post_kind: "post".into(),
            answers: sorted(answers.clone()),
            notify: sorted(notify.clone()),
            idempotency_key: idempotency_key.clone(),
        },
        DiscussCmd::Decision {
            topic,
            body,
            text,
            notify,
            idempotency_key,
        } => Action::Post {
            id: ids::new_post_id(),
            topic: normalize_discussion_topic(topic)?,
            body: require_post_text(text.clone(), body.clone(), "decision")?,
            reply_to: None,
            post_kind: "decision".into(),
            answers: vec![],
            notify: sorted(notify.clone()),
            idempotency_key: idempotency_key.clone(),
        },
        DiscussCmd::Summary {
            topic,
            body,
            text,
            notify,
            idempotency_key,
        } if body.is_some() || text.is_some() => Action::Post {
            id: ids::new_post_id(),
            topic: normalize_discussion_topic(topic)?,
            body: require_post_text(text.clone(), body.clone(), "summary")?,
            reply_to: None,
            post_kind: "summary".into(),
            answers: vec![],
            notify: sorted(notify.clone()),
            idempotency_key: idempotency_key.clone(),
        },
        DiscussCmd::Watch { topic } | DiscussCmd::Unwatch { topic } => Action::Watch {
            topic: normalize_discussion_topic(topic)?,
            watching: matches!(cmd, DiscussCmd::Watch { .. }),
        },
        DiscussCmd::Sticky { post_id } | DiscussCmd::Unsticky { post_id } => Action::Sticky {
            post_id: post_id.clone(),
            sticky: matches!(cmd, DiscussCmd::Sticky { .. }),
        },
        DiscussCmd::Supersede {
            old_post_id,
            new_post_id,
        } => Action::Supersede {
            old_post_id: old_post_id.clone(),
            new_post_id: new_post_id.clone(),
        },
        DiscussCmd::Retract { post_id, reason } => Action::Retract {
            post_id: post_id.clone(),
            reason: reason.clone(),
        },
        DiscussCmd::NeedsBead { post_id, topic } | DiscussCmd::Resolve { post_id, topic } => {
            Action::Route {
                post_id: post_id.clone(),
                topic: topic
                    .as_deref()
                    .map(normalize_discussion_topic)
                    .transpose()?,
                route_state: if matches!(cmd, DiscussCmd::NeedsBead { .. }) {
                    "needs_bead"
                } else {
                    "resolved"
                }
                .into(),
                issue: None,
                note: None,
            }
        }
        DiscussCmd::Route {
            post_id,
            topic,
            issue,
            note,
        } => Action::Route {
            post_id: post_id.clone(),
            topic: topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?,
            route_state: "routed".into(),
            issue: Some(issue.clone()),
            note: resolve_optional_text(note.clone())?,
        },
        DiscussCmd::Topic {
            cmd:
                DiscussTopicCmd::New {
                    topic,
                    title,
                    description,
                    body,
                    notify,
                    idempotency_key,
                },
        } => {
            let body = resolve_optional_text(body.clone())?;
            if body.is_none() && (!notify.is_empty() || idempotency_key.is_some()) {
                return Err(MoteError::Invalid(
                    "--notify and --idempotency-key require an initial post body".into(),
                ));
            }
            Action::Topic {
                topic: normalize_discussion_topic(topic)?,
                title: title.clone(),
                description: resolve_optional_text(description.clone())?,
                post: body.map(|body| InitialPost {
                    id: ids::new_post_id(),
                    body,
                    notify: sorted(notify.clone()),
                    idempotency_key: idempotency_key.clone(),
                }),
            }
        }
        DiscussCmd::MarkRead { topic, through } => {
            let topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let (snapshot, state) = observe(store)?;
            let actor = store.resolve_actor(cli.actor.as_deref())?;
            let target = match through {
                Some(id) => Some(
                    state
                        .board_posts
                        .get(id)
                        .ok_or_else(|| MoteError::Invalid(format!("no such post {id}")))?,
                ),
                None => state
                    .board_posts_for(topic.as_deref())
                    .into_iter()
                    .max_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id)),
            };
            let Some(target) = target else {
                return output(cli, &snapshot, json!({"marked_read":null}));
            };
            if topic.as_ref().is_some_and(|t| t != &target.topic) {
                return Err(MoteError::Invalid(
                    "read target belongs to another topic".into(),
                ));
            }
            Action::Read {
                topic: topic.clone(),
                through_post: target.post_id.clone(),
                expect: state
                    .discussion_cursor_for(&actor, topic.as_deref())
                    .cloned(),
            }
        }
        DiscussCmd::Promote {
            post_id,
            title,
            body,
            priority,
            tags,
            deps,
        } => {
            let (_, state) = observe(store)?;
            let post = state
                .board_posts
                .get(post_id)
                .ok_or_else(|| MoteError::Invalid(format!("no such post {post_id}")))?;
            Action::Promote {
                post_id: post_id.clone(),
                task_id: ids::new_bead_id(),
                title: title
                    .clone()
                    .unwrap_or_else(|| crate::cli::first_line_title(&post.body)),
                body: resolve_optional_text(body.clone())?.unwrap_or_else(|| post.body.clone()),
                priority: priority.unwrap_or(2),
                tags: sorted(tags.clone()),
                deps: sorted(deps.clone()),
            }
        }
        DiscussCmd::Decide {
            topic,
            show: false,
            agreed_post_ids,
            open_questions,
            references: raw_references,
            issues,
            body,
            text,
            notify,
            idempotency_key,
        } => {
            let mut refs = references(raw_references)?;
            refs.extend(issues.iter().map(|id| Reference::Issue {
                issue_id: id.clone(),
            }));
            let refs = sorted(refs);
            let issues = refs
                .iter()
                .filter_map(|r| {
                    if let Reference::Issue { issue_id } = r {
                        Some(issue_id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            Action::Decision {
                id: ids::new_post_id(),
                topic: normalize_discussion_topic(topic)?,
                body: resolve_decision_body(text.clone(), body.clone())?,
                agreed_post_ids: sorted(agreed_post_ids.clone()),
                references: refs,
                open_questions: open_questions
                    .iter()
                    .map(|text| QuestionDraft {
                        question_id: ids::new_question_id(),
                        text: text.clone(),
                    })
                    .collect(),
                notify: sorted(notify.clone()),
                idempotency_key: idempotency_key.clone(),
                issues,
            }
        }
        DiscussCmd::Question { cmd } if !matches!(cmd, DiscussQuestionCmd::Show { .. }) => {
            question_action(cmd)?
        }
        _ => return discussion_read(cli, store, cmd),
    };
    send(cli, store, action)
}

fn question_action(cmd: &DiscussQuestionCmd) -> MoteResult<Action> {
    let (id, action, expect, refs, note, successor, key) = match cmd {
        DiscussQuestionCmd::Answer {
            question_id,
            posts,
            references: raw,
            note,
            expect,
            idempotency_key,
        } => {
            let mut refs = references(raw)?;
            refs.extend(posts.iter().map(|id| Reference::Post {
                post_id: id.clone(),
            }));
            (
                question_id,
                "answer",
                expect,
                sorted(refs),
                resolve_optional_text(note.clone())?,
                None,
                idempotency_key,
            )
        }
        DiscussQuestionCmd::Defer {
            question_id,
            reason,
            expect,
            idempotency_key,
        } => (
            question_id,
            "defer",
            expect,
            vec![],
            resolve_optional_text(Some(reason.clone()))?,
            None,
            idempotency_key,
        ),
        DiscussQuestionCmd::Supersede {
            question_id,
            successor_question_id,
            reason,
            expect,
            idempotency_key,
        } => (
            question_id,
            "supersede",
            expect,
            vec![],
            resolve_optional_text(reason.clone())?,
            Some(successor_question_id.clone()),
            idempotency_key,
        ),
        DiscussQuestionCmd::Close {
            question_id,
            resolution,
            references: raw,
            expect,
            idempotency_key,
        } => (
            question_id,
            "close",
            expect,
            references(raw)?,
            resolve_optional_text(Some(resolution.clone()))?,
            None,
            idempotency_key,
        ),
        DiscussQuestionCmd::Show { .. } => unreachable!("question reads dispatched separately"),
    };
    Ok(Action::Question {
        question_id: id.clone(),
        action: action.into(),
        expect_question: expect.clone(),
        references: refs,
        note,
        successor_question_id: successor,
        idempotency_key: Some(key.clone()),
    })
}

fn post_page(
    state: &State,
    mut posts: Vec<&BoardPostRecord>,
    limit: Option<usize>,
    before: Option<&str>,
    topic: Option<&str>,
    actor: &str,
) -> MoteResult<Value> {
    let snapshot_last = posts.last().copied();
    let mut before_op_id = None;
    if let Some(before) = before {
        let boundary = state
            .board_posts
            .get(before)
            .ok_or_else(|| MoteError::Invalid(format!("no such before post {before}")))?;
        if topic.is_some_and(|topic| topic != boundary.topic) {
            return Err(MoteError::Invalid(
                "before post belongs to another topic".into(),
            ));
        }
        before_op_id = Some(boundary.sent_op_id.as_str());
        posts.retain(|p| p.sent_op_id < boundary.sent_op_id);
    }
    posts.sort_by(|a, b| a.sent_op_id.cmp(&b.sent_op_id));
    let more = limit.is_some_and(|limit| posts.len() > limit);
    if let Some(limit) = limit {
        posts.drain(..posts.len().saturating_sub(limit));
    }
    let first = posts.first().copied();
    let last = posts.last().copied();
    Ok(json!({"posts":post_values(state, &posts),"page":{
        "order":"chronological","window":"newest","topic":topic,"before":before,"limit":limit,
        "count":posts.len(),"has_older":more,"has_newer":before_op_id.is_some_and(|id| snapshot_last.is_some_and(|p| p.sent_op_id.as_str() >= id)),
        "first_post_id":first.map(|p| &p.post_id),"first_op_id":first.map(|p| &p.sent_op_id),
        "last_post_id":last.map(|p| &p.post_id),"last_op_id":last.map(|p| &p.sent_op_id),
        "snapshot_last_post_id":snapshot_last.map(|p| &p.post_id),"snapshot_last_op_id":snapshot_last.map(|p| &p.sent_op_id),
        "effective_cursor_op_id":state.discussion_cursor_for(actor,topic)
    }}))
}

fn discussion_read(cli: &Cli, store: &Store, cmd: &DiscussCmd) -> MoteResult<i32> {
    let (snapshot, state) = observe(store)?;
    let actor = store.resolve_actor(cli.actor.as_deref())?;
    let data = match cmd {
        DiscussCmd::List { topic, limit } => {
            let topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let mut posts = state.board_posts_for(topic.as_deref());
            if let Some(limit) = limit {
                posts = crate::cli::limit_board_posts_preserving_stickies(posts, *limit);
            }
            post_values(&state, &posts)
        }
        DiscussCmd::Unread {
            topic,
            limit,
            before,
            page,
        } => {
            let topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let result = post_page(
                &state,
                state.unread_board_posts_for(&actor, topic.as_deref()),
                *limit,
                before.as_deref(),
                topic.as_deref(),
                &actor,
            )?;
            if *page {
                result
            } else {
                result["posts"].clone()
            }
        }
        DiscussCmd::Notifications {
            topic,
            limit,
            before,
        } => {
            let topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            post_page(
                &state,
                state.unread_board_notifications_for(&actor, topic.as_deref()),
                *limit,
                before.as_deref(),
                topic.as_deref(),
                &actor,
            )?
        }
        DiscussCmd::Watches => json!(state.watched_topics_for(&actor)),
        DiscussCmd::Replies { post_id } | DiscussCmd::Thread { post_id } => {
            if !state.board_posts.contains_key(post_id) {
                return Err(MoteError::Invalid(format!("no such post {post_id}")));
            }
            if matches!(cmd, DiscussCmd::Replies { .. }) {
                post_values(&state, &state.replies_to(post_id))
            } else {
                json!(
                    state
                        .thread_posts(post_id)
                        .into_iter()
                        .map(|(depth, post)| json!({"depth":depth,"post":crate::cli::board_post_json_with_state(&state,post)}))
                        .collect::<Vec<_>>()
                )
            }
        }
        DiscussCmd::Topics => json!(
            state
                .board_topics
                .values()
                .map(|t| crate::cli::topic_json(&state, t))
                .collect::<Vec<_>>()
        ),
        DiscussCmd::Search {
            query,
            topic,
            limit,
        } => {
            let query = query.trim();
            if query.is_empty() {
                return Err(MoteError::Invalid(
                    "discussion search query must be non-empty".into(),
                ));
            }
            let query = query.to_ascii_lowercase();
            let topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            let mut posts: Vec<_> = state
                .board_posts_for(topic.as_deref())
                .into_iter()
                .filter(|p| {
                    p.post_id.to_ascii_lowercase().contains(&query)
                        || p.topic.to_ascii_lowercase().contains(&query)
                        || p.from.to_ascii_lowercase().contains(&query)
                        || p.body.to_ascii_lowercase().contains(&query)
                })
                .collect();
            let mut topics: Vec<_> = state
                .board_topics_by_activity()
                .into_iter()
                .filter(|t| topic.as_ref().is_none_or(|wanted| &t.topic == wanted))
                .filter(|t| {
                    t.topic.to_ascii_lowercase().contains(&query)
                        || t.title.to_ascii_lowercase().contains(&query)
                        || t.body.to_ascii_lowercase().contains(&query)
                })
                .collect();
            if let Some(limit) = limit {
                posts = crate::cli::limit_board_posts_preserving_stickies(posts, *limit);
                topics.truncate(*limit);
            }
            json!({"topics":topics.iter().map(|t| crate::cli::topic_json(&state,t)).collect::<Vec<_>>(),"posts":post_values(&state,&posts)})
        }
        DiscussCmd::Unrouted { topic } => {
            let topic = topic
                .as_deref()
                .map(normalize_discussion_topic)
                .transpose()?;
            json!({"posts":post_values(&state,&state.board_posts_for(topic.as_deref()).into_iter().filter(|p| p.route.state.as_str() == "needs_bead").collect::<Vec<_>>()),
                "topics":state.board_topics.values().filter(|t| t.route.state.as_str() == "needs_bead" && topic.as_ref().is_none_or(|wanted| &t.topic == wanted)).map(|t| crate::cli::topic_json(&state,t)).collect::<Vec<_>>()})
        }
        DiscussCmd::Summary {
            topic,
            notify,
            idempotency_key,
            ..
        } => {
            if !notify.is_empty() || idempotency_key.is_some() {
                return Err(MoteError::Invalid(
                    "--notify and --idempotency-key require summary text".into(),
                ));
            }
            let topic = normalize_discussion_topic(topic)?;
            let record = state
                .board_topics
                .get(&topic)
                .ok_or_else(|| MoteError::Invalid(format!("no such topic {topic}")))?;
            json!({"topic":crate::cli::topic_json(&state,record),"summary":record.summary_post_id.as_ref().and_then(|id| state.board_posts.get(id)).map(|p|crate::cli::board_post_json_with_state(&state,p))})
        }
        DiscussCmd::Decide {
            topic, show: true, ..
        } => {
            let topic = normalize_discussion_topic(topic)?;
            json!({"decisions":state.board_decisions_for_topic(&topic).iter().map(|d|crate::cli::structured_decision_json(&state,d)).collect::<Vec<_>>(),"questions":state.board_questions_for_topic(&topic).iter().map(|q|crate::cli::decision_question_json(q)).collect::<Vec<_>>(),"counts":state.board_question_counts(Some(&topic))})
        }
        DiscussCmd::Question {
            cmd: DiscussQuestionCmd::Show { question_id },
        } => serde_json::to_value(
            state
                .board_questions
                .get(question_id)
                .ok_or_else(|| MoteError::Invalid(format!("no such question {question_id}")))?,
        )?,
        DiscussCmd::Pulse {
            as_of,
            short_window,
            burst_window,
            active_window,
            attention_window,
            burst_posts,
            burst_actors,
        } => {
            let instant = as_of
                .as_deref()
                .map(str::parse::<Timestamp>)
                .transpose()
                .map_err(|e| MoteError::Invalid(e.to_string()))?
                .unwrap_or_else(Timestamp::now);
            let parameters = crate::discussion_pulse::DiscussionPulseParameters {
                short_window_s: *short_window,
                burst_window_s: *burst_window,
                active_window_s: *active_window,
                attention_window_s: *attention_window,
                burst_posts: *burst_posts,
                burst_actors: *burst_actors,
            };
            serde_json::to_value(
                crate::discussion_pulse::build_discussion_pulse(
                    &state,
                    Some(&actor),
                    instant,
                    parameters,
                )
                .map_err(MoteError::Invalid)?,
            )?
        }
        _ => unreachable!("discussion mutation dispatched before reads"),
    };
    output(cli, &snapshot, data)
}

fn inbox(cli: &Cli, store: &Store) -> MoteResult<i32> {
    let Command::Inbox {
        issue,
        from,
        kind,
        follow,
        wait,
        timeout,
        after,
        interval,
    } = &cli.command
    else {
        unreachable!()
    };
    if after.is_some() && !follow {
        return Err(MoteError::Invalid("--after requires --follow".into()));
    }
    if *interval == 0 {
        return Err(MoteError::Invalid("inbox interval must be positive".into()));
    }
    let actor = store.resolve_actor(cli.actor.as_deref())?;
    let (client, _scratch) = open_client(store)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(timeout.unwrap_or(60)))
        .ok_or_else(|| MoteError::Invalid("inbox timeout is too large".into()))?;
    let mut seen = BTreeSet::new();
    loop {
        let snapshot = client.refresh()?;
        let state = snapshot.state.coordination.communication_state()?;
        let rows: Vec<_> = state
            .inbox_for(&actor)
            .into_iter()
            .filter(|m| {
                issue
                    .as_ref()
                    .is_none_or(|id| m.entity.as_ref() == Some(id))
            })
            .filter(|m| from.as_ref().is_none_or(|sender| &m.from == sender))
            .filter(|m| kind.as_ref().is_none_or(|kind| &m.msg_kind == kind))
            .filter(|m| after.as_ref().is_none_or(|after| &m.sent_op_id > after))
            .filter(|m| !seen.contains(&m.msg_id))
            .collect();
        let empty = rows.is_empty();
        if !empty || (!follow && (!wait || Instant::now() >= deadline)) {
            print_value(
                cli.json,
                view_with_data(
                    &snapshot,
                    json!(rows.iter().map(|m| message_value(m)).collect::<Vec<_>>()),
                ),
            );
        }
        seen.extend(rows.iter().map(|m| m.msg_id.clone()));
        if !follow && (!wait || !empty || Instant::now() >= deadline) {
            return Ok(0);
        }
        let pause = Duration::from_secs(*interval);
        std::thread::sleep(if *follow {
            pause
        } else {
            pause.min(deadline.saturating_duration_since(Instant::now()))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_cli_reference_targets_preserve_exact_imported_bytes() {
        assert_eq!(
            references(&["post: post-雪 ".into()]).unwrap(),
            vec![Reference::Post {
                post_id: " post-雪 ".into()
            }]
        );
        assert_eq!(
            references(&["candidate: old candidate ".into()]).unwrap(),
            vec![Reference::Candidate {
                candidate_id: " old candidate ".into()
            }]
        );
        assert_eq!(
            references(&["url:https://example.invalid/a:b".into()]).unwrap(),
            vec![Reference::Url {
                url: "https://example.invalid/a:b".into()
            }]
        );
        assert!(references(&["post:".into()]).is_err());
    }
}
