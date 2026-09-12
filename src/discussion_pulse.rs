//! Deterministic, read-only projection of discussion activity and attention.
//!
//! The pulse deliberately separates recent activity ("active now") from
//! durable obligations ("needs eyes"). A busy topic therefore cannot hide a
//! single unread post, and an old unread post does not disappear merely because
//! it aged out of the activity window.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::state::{BoardPostRecord, State};

pub const DISCUSSION_PULSE_SCHEMA: &str = "mote.discussion-pulse.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiscussionPulseDefinitions {
    pub active: &'static str,
    pub burst: &'static str,
    pub solitary_new: &'static str,
    pub unread: &'static str,
    pub explicit_attention: &'static str,
    pub no_external_reply: &'static str,
}

impl Default for DiscussionPulseDefinitions {
    fn default() -> Self {
        Self {
            active: "at least one current active post inside active_window_s",
            burst: "at least burst_posts current active posts by at least burst_actors distinct authors inside burst_window_s",
            solitary_new: "exactly one current active external post in the topic inside active_window_s, newer than the actor's effective cursor, with no descendant reply by another actor",
            unread: "current active external post newer than the actor's effective global/topic cursor",
            explicit_attention: "unread notification, watched-topic unread, unresolved structured question, or needs_bead route state",
            no_external_reply: "current active top-level post by the actor inside attention_window_s with no descendant reply by another actor; self-replies do not clear it",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DiscussionPulseParameters {
    pub short_window_s: u32,
    pub burst_window_s: u32,
    pub active_window_s: u32,
    pub attention_window_s: u32,
    pub burst_posts: usize,
    pub burst_actors: usize,
}

impl Default for DiscussionPulseParameters {
    fn default() -> Self {
        Self {
            short_window_s: 5 * 60,
            burst_window_s: 15 * 60,
            active_window_s: 60 * 60,
            attention_window_s: 60 * 60,
            burst_posts: 4,
            burst_actors: 2,
        }
    }
}

impl DiscussionPulseParameters {
    pub fn validate(self) -> Result<Self, String> {
        if self.short_window_s == 0
            || self.burst_window_s == 0
            || self.active_window_s == 0
            || self.attention_window_s == 0
        {
            return Err("discussion pulse windows must be greater than zero".into());
        }
        if self.burst_posts == 0 || self.burst_actors == 0 {
            return Err("discussion pulse burst thresholds must be greater than zero".into());
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PulsePostRef {
    pub post_id: String,
    pub sent_ts: String,
    pub sent_op_id: String,
}

impl PulsePostRef {
    fn from_post(post: &BoardPostRecord) -> Self {
        Self {
            post_id: post.post_id.clone(),
            sent_ts: post.sent_ts.clone(),
            sent_op_id: post.sent_op_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopicActivityPulse {
    pub topic: String,
    pub title: String,
    pub posts_5m: usize,
    pub posts_15m: usize,
    pub posts_60m: usize,
    pub distinct_authors_60m: usize,
    pub top_level_60m: usize,
    pub replies_60m: usize,
    pub raw_posts_60m: usize,
    pub active_posts_60m: usize,
    pub retracted_posts_60m: usize,
    pub superseded_posts_60m: usize,
    pub posts_short_window: usize,
    pub posts_burst_window: usize,
    pub distinct_authors_burst_window: usize,
    pub posts_active_window: usize,
    pub distinct_authors_active_window: usize,
    pub burst: bool,
    pub last_post_id: String,
    pub last_activity_ts: String,
    pub last_post: PulsePostRef,
    /// Latest active reply by someone other than the resolved actor. With no
    /// actor, this is simply the latest active reply in the topic.
    pub last_external_reply: Option<PulsePostRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TopicAttentionPulse {
    pub topic: String,
    pub title: String,
    pub unread_count: usize,
    pub oldest_unread: Option<PulsePostRef>,
    pub newest_unread: Option<PulsePostRef>,
    pub notification_count: usize,
    pub explicit_notification_count: usize,
    pub watched_unread_count: usize,
    pub unresolved_question_count: usize,
    pub needs_bead_count: usize,
    pub solitary_new_post_ids: Vec<String>,
    pub no_external_reply_post_ids: Vec<String>,
    pub explicit_attention: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DiscussionPulseTotals {
    pub active_topics: usize,
    pub burst_topics: usize,
    pub needs_eyes_topics: usize,
    pub unread_posts: usize,
    pub solitary_new_posts: usize,
    pub no_external_reply_posts: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DiscussionPulseTraversal {
    pub topics_scanned: usize,
    pub posts_scanned: usize,
    pub reply_edges_scanned: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscussionPulse {
    pub schema: &'static str,
    pub actor: Option<String>,
    pub as_of_ts: String,
    pub parameters: DiscussionPulseParameters,
    pub definitions: DiscussionPulseDefinitions,
    pub active_now: Vec<TopicActivityPulse>,
    pub needs_eyes: Vec<TopicAttentionPulse>,
    pub totals: DiscussionPulseTotals,
    pub traversal: DiscussionPulseTraversal,
}

#[derive(Default)]
struct TopicAccumulator {
    title: String,
    posts_5m: usize,
    posts_15m: usize,
    posts_60m: usize,
    authors_60m: BTreeSet<String>,
    top_level_60m: usize,
    replies_60m: usize,
    raw_posts_60m: usize,
    retracted_posts_60m: usize,
    superseded_posts_60m: usize,
    posts_short_window: usize,
    posts_burst_window: usize,
    authors_burst_window: BTreeSet<String>,
    posts_active_window: usize,
    authors_active_window: BTreeSet<String>,
    last_active: Option<PulsePostRef>,
    unread: Vec<PulsePostRef>,
    notification_count: usize,
    explicit_notification_count: usize,
    last_external_reply: Option<PulsePostRef>,
    recent_external: Vec<String>,
    recent_own_roots: Vec<String>,
    needs_bead_count: usize,
}

#[derive(Clone)]
struct ReplyNode {
    topic: String,
    author: String,
    parent: Option<String>,
    remaining_children: usize,
    mixed_authors: bool,
}

/// Build the canonical discussion pulse at an injected instant.
///
/// This function is passive: it reduces no operation, advances no read cursor,
/// and mutates neither the store nor the supplied state.
pub fn build_discussion_pulse(
    state: &State,
    actor: Option<&str>,
    as_of: Timestamp,
    parameters: DiscussionPulseParameters,
) -> Result<DiscussionPulse, String> {
    let parameters = parameters.validate()?;
    let as_of_ts = crate::ids::format_rfc3339(as_of);
    let five_minutes = 5 * 60;
    let fifteen_minutes = 15 * 60;
    let sixty_minutes = 60 * 60;
    let watched: BTreeSet<String> = actor
        .map(|name| state.watched_topics_for(name).into_iter().collect())
        .unwrap_or_default();

    let mut topics: BTreeMap<String, TopicAccumulator> = state
        .board_topics
        .iter()
        .map(|(topic, record)| {
            let mut accumulator = TopicAccumulator {
                title: record.title.clone(),
                ..TopicAccumulator::default()
            };
            if record.route.needs_action() {
                accumulator.needs_bead_count = 1;
            }
            (topic.clone(), accumulator)
        })
        .collect();
    let mut reply_nodes = BTreeMap::<String, ReplyNode>::new();
    let mut traversal = DiscussionPulseTraversal {
        topics_scanned: state.board_topics.len(),
        ..DiscussionPulseTraversal::default()
    };

    for post in state.board_posts.values() {
        traversal.posts_scanned += 1;
        let Ok(sent) = post.sent_ts.parse::<Timestamp>() else {
            continue;
        };
        if sent > as_of {
            continue;
        }

        let accumulator = topics
            .entry(post.topic.clone())
            .or_insert_with(|| TopicAccumulator {
                title: post.topic.clone(),
                ..TopicAccumulator::default()
            });
        if within(sent, as_of, sixty_minutes) {
            accumulator.raw_posts_60m += 1;
            match post.disposition() {
                "retracted" => accumulator.retracted_posts_60m += 1,
                "superseded" => accumulator.superseded_posts_60m += 1,
                _ => {}
            }
        }
        if post.disposition() != "active" {
            continue;
        }

        reply_nodes.insert(
            post.post_id.clone(),
            ReplyNode {
                topic: post.topic.clone(),
                author: post.from.clone(),
                parent: post.reply_to.clone(),
                remaining_children: 0,
                mixed_authors: false,
            },
        );

        let in_5m = within(sent, as_of, five_minutes);
        let in_15m = within(sent, as_of, fifteen_minutes);
        let in_60m = within(sent, as_of, sixty_minutes);
        let in_short = within(sent, as_of, parameters.short_window_s);
        let in_burst = within(sent, as_of, parameters.burst_window_s);
        let in_active = within(sent, as_of, parameters.active_window_s);
        let in_attention = within(sent, as_of, parameters.attention_window_s);

        if in_5m {
            accumulator.posts_5m += 1;
        }
        if in_15m {
            accumulator.posts_15m += 1;
        }
        if in_60m {
            accumulator.posts_60m += 1;
            accumulator.authors_60m.insert(post.from.clone());
            if post.reply_to.is_some() {
                accumulator.replies_60m += 1;
            } else {
                accumulator.top_level_60m += 1;
            }
        }
        if in_short {
            accumulator.posts_short_window += 1;
        }
        if in_burst {
            accumulator.posts_burst_window += 1;
            accumulator.authors_burst_window.insert(post.from.clone());
        }
        if in_active {
            accumulator.posts_active_window += 1;
            accumulator.authors_active_window.insert(post.from.clone());
            let current = PulsePostRef::from_post(post);
            if accumulator
                .last_active
                .as_ref()
                .is_none_or(|last| last.sent_op_id < current.sent_op_id)
            {
                accumulator.last_active = Some(current);
            }
        }

        if post.route.needs_action() {
            accumulator.needs_bead_count += 1;
        }

        if post.reply_to.is_some() && actor.is_none_or(|name| post.from != name) {
            let current = PulsePostRef::from_post(post);
            if accumulator
                .last_external_reply
                .as_ref()
                .is_none_or(|last| last.sent_op_id < current.sent_op_id)
            {
                accumulator.last_external_reply = Some(current);
            }
        }

        if let Some(actor) = actor {
            let cursor = state
                .discussion_cursor_for(actor, Some(&post.topic))
                .map(String::as_str)
                .unwrap_or("");
            let unread = post.from != actor && post.sent_op_id.as_str() > cursor;
            if unread {
                accumulator.unread.push(PulsePostRef::from_post(post));
                if post
                    .notification_recipients
                    .iter()
                    .any(|name| name == actor)
                {
                    accumulator.notification_count += 1;
                }
                if post.explicit_notify.iter().any(|name| name == actor) {
                    accumulator.explicit_notification_count += 1;
                }
            }
            if in_active && post.from != actor {
                accumulator.recent_external.push(post.post_id.clone());
            }
            if in_attention && post.from == actor && post.reply_to.is_none() {
                accumulator.recent_own_roots.push(post.post_id.clone());
            }
        }
    }

    // Fold each active reply forest from its leaves. The summary retained for
    // a post is whether any active descendant has a different author. This is
    // linear in posts plus reply edges and does not rescan a topic per post.
    let edges = reply_nodes
        .iter()
        .filter_map(|(post_id, node)| {
            let parent = node.parent.as_ref()?;
            let parent_node = reply_nodes.get(parent)?;
            (parent_node.topic == node.topic).then(|| (post_id.clone(), parent.clone()))
        })
        .collect::<Vec<_>>();
    traversal.reply_edges_scanned = edges.len();
    for (_, parent) in &edges {
        if let Some(node) = reply_nodes.get_mut(parent) {
            node.remaining_children += 1;
        }
    }
    let mut parents = BTreeMap::<String, String>::new();
    for (child, parent) in edges {
        parents.insert(child, parent);
    }
    let mut queue = reply_nodes
        .iter()
        .filter(|(_, node)| node.remaining_children == 0)
        .map(|(post_id, _)| post_id.clone())
        .collect::<VecDeque<_>>();
    let mut processed = BTreeSet::new();
    while let Some(post_id) = queue.pop_front() {
        processed.insert(post_id.clone());
        let Some(parent_id) = parents.get(&post_id).cloned() else {
            continue;
        };
        let Some(child) = reply_nodes.get(&post_id).cloned() else {
            continue;
        };
        let Some(parent) = reply_nodes.get_mut(&parent_id) else {
            continue;
        };
        parent.mixed_authors |= child.mixed_authors || child.author != parent.author;
        parent.remaining_children = parent.remaining_children.saturating_sub(1);
        if parent.remaining_children == 0 {
            queue.push_back(parent_id);
        }
    }
    // Cyclic reply data should not occur in accepted state. If encountered,
    // classify it conservatively as replied-to rather than claiming neglect.
    for (post_id, node) in &mut reply_nodes {
        if !processed.contains(post_id) && node.remaining_children > 0 {
            node.mixed_authors = true;
        }
    }

    let mut active_now = Vec::new();
    let mut needs_eyes = Vec::new();
    for (topic, mut accumulator) in topics {
        if accumulator.posts_active_window > 0 {
            let last_post = accumulator
                .last_active
                .take()
                .expect("an active topic has a most recent post");
            active_now.push(TopicActivityPulse {
                topic: topic.clone(),
                title: accumulator.title.clone(),
                posts_5m: accumulator.posts_5m,
                posts_15m: accumulator.posts_15m,
                posts_60m: accumulator.posts_60m,
                distinct_authors_60m: accumulator.authors_60m.len(),
                top_level_60m: accumulator.top_level_60m,
                replies_60m: accumulator.replies_60m,
                raw_posts_60m: accumulator.raw_posts_60m,
                active_posts_60m: accumulator.posts_60m,
                retracted_posts_60m: accumulator.retracted_posts_60m,
                superseded_posts_60m: accumulator.superseded_posts_60m,
                posts_short_window: accumulator.posts_short_window,
                posts_burst_window: accumulator.posts_burst_window,
                distinct_authors_burst_window: accumulator.authors_burst_window.len(),
                posts_active_window: accumulator.posts_active_window,
                distinct_authors_active_window: accumulator.authors_active_window.len(),
                burst: accumulator.posts_burst_window >= parameters.burst_posts
                    && accumulator.authors_burst_window.len() >= parameters.burst_actors,
                last_post_id: last_post.post_id.clone(),
                last_activity_ts: last_post.sent_ts.clone(),
                last_post,
                last_external_reply: accumulator.last_external_reply.clone(),
            });
        }

        accumulator
            .unread
            .sort_by(|left, right| left.sent_op_id.cmp(&right.sent_op_id));
        let unread_ids = accumulator
            .unread
            .iter()
            .map(|post| post.post_id.as_str())
            .collect::<BTreeSet<_>>();
        let solitary_new_post_ids = if accumulator.recent_external.len() == 1
            && unread_ids.contains(accumulator.recent_external[0].as_str())
            && !reply_nodes
                .get(&accumulator.recent_external[0])
                .is_some_and(|node| node.mixed_authors)
        {
            accumulator.recent_external.clone()
        } else {
            Vec::new()
        };
        let mut no_external_reply_post_ids = accumulator
            .recent_own_roots
            .iter()
            .filter(|post_id| {
                !reply_nodes
                    .get(*post_id)
                    .is_some_and(|node| node.mixed_authors)
            })
            .cloned()
            .collect::<Vec<_>>();
        no_external_reply_post_ids.sort();
        let watched_unread_count = if watched.contains(&topic) {
            accumulator.unread.len()
        } else {
            0
        };
        let unresolved_question_count = state.board_question_counts(Some(&topic)).unresolved;
        let explicit_attention = accumulator.notification_count > 0
            || watched_unread_count > 0
            || unresolved_question_count > 0
            || accumulator.needs_bead_count > 0;
        let has_attention = !accumulator.unread.is_empty()
            || explicit_attention
            || !solitary_new_post_ids.is_empty()
            || !no_external_reply_post_ids.is_empty();
        if has_attention {
            needs_eyes.push(TopicAttentionPulse {
                topic,
                title: accumulator.title,
                unread_count: accumulator.unread.len(),
                oldest_unread: accumulator.unread.first().cloned(),
                newest_unread: accumulator.unread.last().cloned(),
                notification_count: accumulator.notification_count,
                explicit_notification_count: accumulator.explicit_notification_count,
                watched_unread_count,
                unresolved_question_count,
                needs_bead_count: accumulator.needs_bead_count,
                solitary_new_post_ids,
                no_external_reply_post_ids,
                explicit_attention,
            });
        }
    }

    active_now.sort_by(|left, right| {
        right
            .last_post
            .sent_op_id
            .cmp(&left.last_post.sent_op_id)
            .then_with(|| left.topic.cmp(&right.topic))
    });
    needs_eyes.sort_by(|left, right| {
        right
            .explicit_attention
            .cmp(&left.explicit_attention)
            .then_with(|| {
                let left_oldest = left
                    .oldest_unread
                    .as_ref()
                    .map(|post| post.sent_op_id.as_str());
                let right_oldest = right
                    .oldest_unread
                    .as_ref()
                    .map(|post| post.sent_op_id.as_str());
                match (left_oldest, right_oldest) {
                    (Some(left), Some(right)) => left.cmp(right),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                }
            })
            .then_with(|| left.topic.cmp(&right.topic))
    });

    let totals = DiscussionPulseTotals {
        active_topics: active_now.len(),
        burst_topics: active_now.iter().filter(|topic| topic.burst).count(),
        needs_eyes_topics: needs_eyes.len(),
        unread_posts: needs_eyes.iter().map(|topic| topic.unread_count).sum(),
        solitary_new_posts: needs_eyes
            .iter()
            .map(|topic| topic.solitary_new_post_ids.len())
            .sum(),
        no_external_reply_posts: needs_eyes
            .iter()
            .map(|topic| topic.no_external_reply_post_ids.len())
            .sum(),
    };

    Ok(DiscussionPulse {
        schema: DISCUSSION_PULSE_SCHEMA,
        actor: actor.map(ToOwned::to_owned),
        as_of_ts,
        parameters,
        definitions: DiscussionPulseDefinitions::default(),
        active_now,
        needs_eyes,
        totals,
        traversal,
    })
}

fn within(sent: Timestamp, as_of: Timestamp, seconds: u32) -> bool {
    let cutoff = as_of
        .checked_sub(SignedDuration::from_secs(seconds.into()))
        .expect("bounded discussion pulse window");
    sent >= cutoff && sent <= as_of
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        BoardPostRecord, BoardTopicRecord, BoardTopicWatchRecord, DecisionQuestionRecord,
        DecisionQuestionStatus, RouteRecord, RouteState,
    };

    const AS_OF: &str = "2026-09-12T12:00:00Z";

    fn topic(name: &str) -> BoardTopicRecord {
        BoardTopicRecord {
            topic: name.into(),
            title: name.into(),
            body: String::new(),
            created_by: "alice".into(),
            created_ts: "2026-09-12T10:00:00Z".into(),
            created_op_id: format!("op-topic-{name}"),
            explicit: true,
            last_activity_ts: "2026-09-12T10:00:00Z".into(),
            last_activity_op_id: format!("op-topic-{name}"),
            post_count: 0,
            sticky_count: 0,
            decision_count: 0,
            summary_post_id: None,
            route: RouteRecord::default(),
        }
    }

    fn post(
        id: &str,
        author: &str,
        topic: &str,
        ts: &str,
        op: &str,
        reply_to: Option<&str>,
    ) -> BoardPostRecord {
        BoardPostRecord {
            post_id: id.into(),
            from: author.into(),
            topic: topic.into(),
            body: id.into(),
            reply_to: reply_to.map(Into::into),
            post_kind: "note".into(),
            answers: Vec::new(),
            explicit_notify: Vec::new(),
            notification_recipients: Vec::new(),
            idempotency_key: None,
            sticky: false,
            sticky_op_id: None,
            superseded_by: None,
            superseded_op_id: None,
            supersedes: Vec::new(),
            retracted: false,
            retraction_reason: None,
            retracted_op_id: None,
            route: RouteRecord::default(),
            sent_ts: ts.into(),
            sent_op_id: op.into(),
        }
    }

    fn pulse(state: &State) -> DiscussionPulse {
        build_discussion_pulse(
            state,
            Some("viewer"),
            AS_OF.parse().unwrap(),
            DiscussionPulseParameters::default(),
        )
        .unwrap()
    }

    #[test]
    fn busy_and_solitary_topics_remain_visible_in_separate_lanes() {
        let mut state = State::default();
        state.board_topics.insert("busy".into(), topic("busy"));
        state.board_topics.insert("quiet".into(), topic("quiet"));
        for index in 0..100 {
            let id = format!("busy-{index:03}");
            state.board_posts.insert(
                id.clone(),
                post(
                    &id,
                    if index % 2 == 0 { "alice" } else { "bob" },
                    "busy",
                    "2026-09-12T11:55:00Z",
                    &format!("op-busy-{index:03}"),
                    None,
                ),
            );
        }
        state.board_posts.insert(
            "quiet-one".into(),
            post(
                "quiet-one",
                "carol",
                "quiet",
                "2026-09-12T11:59:00Z",
                "op-quiet-one",
                None,
            ),
        );

        let pulse = pulse(&state);
        assert_eq!(pulse.active_now.len(), 2);
        assert!(pulse.active_now.iter().any(|topic| topic.burst));
        let quiet = pulse
            .needs_eyes
            .iter()
            .find(|topic| topic.topic == "quiet")
            .unwrap();
        assert_eq!(quiet.solitary_new_post_ids, ["quiet-one"]);
        assert_eq!(pulse.traversal.posts_scanned, 101);
    }

    #[test]
    fn one_actor_cannot_create_a_burst() {
        let mut state = State::default();
        state.board_topics.insert("solo".into(), topic("solo"));
        for index in 0..8 {
            let id = format!("post-{index}");
            state.board_posts.insert(
                id.clone(),
                post(
                    &id,
                    "alice",
                    "solo",
                    "2026-09-12T11:55:00Z",
                    &format!("op-{index}"),
                    None,
                ),
            );
        }
        assert!(!pulse(&state).active_now[0].burst);
    }

    #[test]
    fn self_replies_do_not_clear_no_external_reply() {
        let mut state = State::default();
        state.board_topics.insert("thread".into(), topic("thread"));
        state.board_posts.insert(
            "root".into(),
            post(
                "root",
                "viewer",
                "thread",
                "2026-09-12T11:50:00Z",
                "op-1",
                None,
            ),
        );
        state.board_posts.insert(
            "self-reply".into(),
            post(
                "self-reply",
                "viewer",
                "thread",
                "2026-09-12T11:51:00Z",
                "op-2",
                Some("root"),
            ),
        );
        let first = pulse(&state);
        assert_eq!(first.needs_eyes[0].no_external_reply_post_ids, ["root"]);

        state.board_posts.insert(
            "external".into(),
            post(
                "external",
                "alice",
                "thread",
                "2026-09-12T11:52:00Z",
                "op-3",
                Some("self-reply"),
            ),
        );
        let second = pulse(&state);
        assert!(second.needs_eyes[0].no_external_reply_post_ids.is_empty());
        assert_eq!(
            second.active_now[0]
                .last_external_reply
                .as_ref()
                .map(|post| post.post_id.as_str()),
            Some("external")
        );
    }

    #[test]
    fn old_unread_survives_activity_window_and_boundary_is_inclusive() {
        let mut state = State::default();
        state.board_topics.insert("old".into(), topic("old"));
        state.board_topics.insert("edge".into(), topic("edge"));
        state.board_posts.insert(
            "old-post".into(),
            post(
                "old-post",
                "alice",
                "old",
                "2026-09-12T08:00:00Z",
                "op-old",
                None,
            ),
        );
        state.board_posts.insert(
            "edge-post".into(),
            post(
                "edge-post",
                "alice",
                "edge",
                "2026-09-12T11:00:00Z",
                "op-edge",
                None,
            ),
        );

        let pulse = pulse(&state);
        assert_eq!(pulse.active_now.len(), 1);
        assert_eq!(pulse.active_now[0].topic, "edge");
        assert!(pulse.needs_eyes.iter().any(|topic| topic.topic == "old"));
    }

    #[test]
    fn retracted_and_superseded_posts_are_not_current_activity() {
        let mut state = State::default();
        state
            .board_topics
            .insert("history".into(), topic("history"));
        let mut retracted = post(
            "retracted",
            "alice",
            "history",
            "2026-09-12T11:55:00Z",
            "op-1",
            None,
        );
        retracted.retracted = true;
        let mut superseded = post(
            "superseded",
            "alice",
            "history",
            "2026-09-12T11:56:00Z",
            "op-2",
            None,
        );
        superseded.superseded_by = Some("new".into());
        state
            .board_posts
            .insert(retracted.post_id.clone(), retracted);
        state
            .board_posts
            .insert(superseded.post_id.clone(), superseded);

        let historical_only = pulse(&state);
        assert!(historical_only.active_now.is_empty());
        assert!(historical_only.needs_eyes.is_empty());

        state.board_posts.insert(
            "current".into(),
            post(
                "current",
                "viewer",
                "history",
                "2026-09-12T11:57:00Z",
                "op-3",
                Some("retracted"),
            ),
        );
        let pulse = pulse(&state);
        let activity = &pulse.active_now[0];
        assert_eq!(activity.raw_posts_60m, 3);
        assert_eq!(activity.active_posts_60m, 1);
        assert_eq!(activity.retracted_posts_60m, 1);
        assert_eq!(activity.superseded_posts_60m, 1);
        assert_eq!(activity.posts_60m, 1);
    }

    #[test]
    fn solitary_new_respects_cursor_self_post_and_first_external_reply_boundaries() {
        let mut state = State::default();
        state.board_topics.insert("edge".into(), topic("edge"));
        state.board_posts.insert(
            "external-root".into(),
            post(
                "external-root",
                "alice",
                "edge",
                "2026-09-12T11:55:00Z",
                "op-1",
                None,
            ),
        );
        assert_eq!(
            pulse(&state).needs_eyes[0].solitary_new_post_ids,
            ["external-root"]
        );

        // Equality is read: unread is strictly newer than the effective cursor.
        state
            .board_read_cursors
            .insert("viewer".into(), "op-1".into());
        assert!(pulse(&state).needs_eyes.is_empty());
        state.board_read_cursors.clear();

        // The viewer's own post is never unread or solitary-new.
        let mut self_only = State::default();
        self_only.board_topics.insert("self".into(), topic("self"));
        self_only.board_posts.insert(
            "self-root".into(),
            post(
                "self-root",
                "viewer",
                "self",
                "2026-09-12T11:55:00Z",
                "op-self",
                None,
            ),
        );
        let self_pulse = pulse(&self_only);
        assert_eq!(self_pulse.needs_eyes[0].unread_count, 0);
        assert!(self_pulse.needs_eyes[0].solitary_new_post_ids.is_empty());
        assert_eq!(
            self_pulse.needs_eyes[0].no_external_reply_post_ids,
            ["self-root"]
        );

        // The first reply by another author clears solitary-new even when that
        // reply is the viewer's own post and therefore not another external.
        state.board_posts.insert(
            "viewer-reply".into(),
            post(
                "viewer-reply",
                "viewer",
                "edge",
                "2026-09-12T11:56:00Z",
                "op-2",
                Some("external-root"),
            ),
        );
        let replied = pulse(&state);
        assert!(replied.needs_eyes[0].solitary_new_post_ids.is_empty());
        assert_eq!(replied.needs_eyes[0].unread_count, 1);
    }

    #[test]
    fn explicit_attention_dimensions_survive_mark_read_until_their_own_lifecycle_closes() {
        let mut state = State::default();
        state
            .board_topics
            .insert("attention".into(), topic("attention"));
        let mut notified = post(
            "notified",
            "alice",
            "attention",
            "2026-09-12T11:55:00Z",
            "op-1",
            None,
        );
        notified.explicit_notify = vec!["viewer".into()];
        notified.notification_recipients = vec!["viewer".into()];
        let mut routed = post(
            "needs-work",
            "bob",
            "attention",
            "2026-09-12T11:56:00Z",
            "op-2",
            None,
        );
        routed.route.state = RouteState::NeedsBead;
        state.board_posts.insert(notified.post_id.clone(), notified);
        state.board_posts.insert(routed.post_id.clone(), routed);
        state.board_topic_watches.insert(
            ("viewer".into(), "attention".into()),
            BoardTopicWatchRecord {
                actor: "viewer".into(),
                topic: "attention".into(),
                watching: true,
                updated_ts: "2026-09-12T11:00:00Z".into(),
                updated_op_id: "op-watch".into(),
            },
        );
        state.board_questions.insert(
            "question-1".into(),
            DecisionQuestionRecord {
                question_id: "question-1".into(),
                decision_id: "decision-1".into(),
                topic: "attention".into(),
                text: "What next?".into(),
                opened_by: "alice".into(),
                opened_op_id: "op-question".into(),
                opened_ts: "2026-09-12T11:00:00Z".into(),
                position: 0,
                status: DecisionQuestionStatus::Open,
                clock_op_id: "op-question".into(),
                successor_question_id: None,
                transitions: Vec::new(),
            },
        );

        let before_read = pulse(&state);
        let attention = &before_read.needs_eyes[0];
        assert_eq!(attention.unread_count, 2);
        assert_eq!(attention.notification_count, 1);
        assert_eq!(attention.explicit_notification_count, 1);
        assert_eq!(attention.watched_unread_count, 2);
        assert_eq!(attention.unresolved_question_count, 1);
        assert_eq!(attention.needs_bead_count, 1);

        state
            .board_read_cursors
            .insert("viewer".into(), "op-2".into());
        let after_read = pulse(&state);
        let attention = &after_read.needs_eyes[0];
        assert_eq!(attention.unread_count, 0);
        assert_eq!(attention.notification_count, 0);
        assert_eq!(attention.watched_unread_count, 0);
        assert_eq!(attention.unresolved_question_count, 1);
        assert_eq!(attention.needs_bead_count, 1);
        assert!(attention.explicit_attention);
    }
}
