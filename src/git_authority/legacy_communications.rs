//! Operational eligibility for the distinct migrated communications profile.
//! Historical bodies and rejected inputs are preserved without sanitization.
use super::coordination::communications::{self, Action};
use crate::{MoteError, MoteResult, state::State};

pub(super) fn validate(verified: &super::legacy::Verified) -> MoteResult<()> {
    validate_state(&verified.entries, &verified.state)
}
fn opaque(value: &str) -> MoteResult<()> {
    communications::validate(&Action::MessageAck { id: value.into() })
}
fn topic(value: &str) -> MoteResult<()> {
    communications::validate(&Action::Watch {
        topic: value.into(),
        watching: false,
    })
}
fn actor(value: &str) -> MoteResult<()> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(MoteError::Invalid(
            "legacy communication actor cannot be addressed by the new profile".into(),
        ));
    }
    Ok(())
}
fn validate_state(entries: &[(String, Vec<u8>)], state: &State) -> MoteResult<()> {
    if let Some(maximum) = entries
        .iter()
        .map(|(name, _)| name.strip_suffix(".json").unwrap_or(name))
        .max()
    {
        // The largest possible request effect suffix must still round-trip as
        // a cursor. The exact maximum includes rejected source inputs.
        opaque(&format!(
            "{maximum}~gaop-{:020}-999999-eff-{}-999999",
            u64::MAX,
            "r".repeat(100)
        ))?;
    }
    for message in state.messages.values() {
        opaque(&message.msg_id)?;
        actor(&message.from)?;
        actor(&message.to)?;
    }
    for post in state.board_posts.values() {
        opaque(&post.post_id)?;
        opaque(&post.sent_op_id)?;
        actor(&post.from)?;
        topic(&post.topic)?;
    }
    for question in state.board_questions.values() {
        opaque(&question.question_id)?;
        opaque(&question.clock_op_id)?;
        actor(&question.opened_by)?;
        topic(&question.topic)?;
    }
    for record in state.board_topics.values() {
        topic(&record.topic)?;
        actor(&record.created_by)?;
    }
    for (name, cursor) in &state.board_read_cursors {
        actor(name)?;
        opaque(cursor)?;
    }
    for ((name, name_topic), cursor) in &state.board_topic_read_cursors {
        actor(name)?;
        topic(name_topic)?;
        opaque(cursor)?;
    }
    for watch in state.board_topic_watches.values() {
        actor(&watch.actor)?;
        topic(&watch.topic)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn entry(id: &str, kind: &str, fields: serde_json::Value) -> (String, Vec<u8>) {
        let mut value =
            json!({"v":1,"op":id,"kind":kind,"actor":"alice","ts":"2026-10-20T00:00:00Z"});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        (format!("{id}.json"), serde_json::to_vec(&value).unwrap())
    }
    fn eligibility(entries: Vec<(String, Vec<u8>)>) -> MoteResult<()> {
        let state = crate::reducer::replay(entries.clone());
        validate_state(&entries, &state)
    }
    #[test]
    fn rejected_maximum_is_checked_but_nonmaximum_rejected_history_is_preserved() {
        let rejected = entry("z\u{1}", "future_unknown", json!({}));
        assert!(eligibility(vec![rejected.clone()]).is_err());
        assert!(eligibility(vec![rejected, entry("雪", "future_unknown", json!({}))]).is_ok());
    }
    #[test]
    fn accepted_post_identifiers_and_earlier_clocks_must_round_trip() {
        for (id, post) in [("a", "post-\u{1}"), ("a\u{1}", "post-valid")] {
            let entries = vec![
                entry(
                    id,
                    "board_post",
                    json!({"post_id":post,"topic":"design","body":"preserved"}),
                ),
                entry("雪", "future_unknown", json!({})),
            ];
            assert!(
                crate::reducer::replay(entries.clone())
                    .board_posts
                    .contains_key(post)
            );
            assert!(eligibility(entries).is_err());
        }
    }
    #[test]
    fn valid_unicode_names_ids_topics_and_cursors_remain_exact() {
        let entries = vec![
            entry(
                "雪",
                "board_post",
                json!({"post_id":"post-雪","topic":"設計","body":"preserved\u{1} body"}),
            ),
            entry(
                "雪z",
                "board_read",
                json!({"upto_op_id":"雪","topic":"設計"}),
            ),
        ];
        let state = crate::reducer::replay(entries.clone());
        assert_eq!(state.board_posts["post-雪"].sent_op_id, "雪");
        assert_eq!(
            state.discussion_cursor_for("alice", Some("設計")).unwrap(),
            "雪"
        );
        validate_state(&entries, &state).unwrap();
    }
}
