//! Atomic begin plus one deterministic public announcement.
use super::{Action, CoordinationPlan, CoordinationState, communications, lifecycle};
use crate::MoteResult;
use crate::git_authority::protocol::{self, Effect, OutcomeStatus, Request};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announcement {
    pub id: String,
    pub topic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnouncedBegin {
    pub begin: lifecycle::Begin,
    pub announcement: Announcement,
}

fn post(action: &AnnouncedBegin) -> communications::Action {
    let begin = &action.begin;
    let body = match &begin.reservation {
        Some(reservation) => format!(
            "claiming {} for {} on {}",
            begin.id,
            reservation,
            begin.paths.join(", ")
        ),
        None => format!("claiming {}", begin.id),
    };
    communications::Action::Post {
        id: action.announcement.id.clone(),
        topic: action.announcement.topic.clone(),
        body,
        reply_to: None,
        post_kind: "post".into(),
        answers: vec![],
        notify: vec![],
        idempotency_key: None,
    }
}

pub(super) fn validate(action: &AnnouncedBegin) -> MoteResult<()> {
    lifecycle::validate(&Action::Begin(action.begin.clone()))?;
    // Include the generated body: otherwise valid long paths could overflow
    // the communication payload only after ownership had been provisionally set.
    communications::validate(&post(action))
}

pub(super) fn plan(
    state: &CoordinationState,
    request: &Request,
    sequence: u64,
    action: &AnnouncedBegin,
) -> MoteResult<CoordinationPlan> {
    // This clone is only an input to the existing pure Begin planner. The
    // outer request remains the sole journaled/digested canonical intent.
    let mut inner = request.clone();
    inner.command = protocol::Command::Coordination {
        action: Action::Begin(action.begin.clone()),
    };
    let begun = lifecycle::plan(state, &inner, sequence)?;
    if begun.outcome.status == OutcomeStatus::Rejected {
        return Ok(begun);
    }
    let mut announced = communications::plan_at(
        &begun.next_state,
        request,
        sequence,
        &post(action),
        begun.effects.len(),
    )?;
    if announced.outcome.status == OutcomeStatus::Rejected {
        // The communication planner saw provisional begin state; none of that
        // state or its effects may escape a failed compound command.
        announced.next_state = state.clone();
        return Ok(announced);
    }
    let mut effects = begun.effects;
    effects.append(&mut announced.effects);
    let mut outcome = begun.outcome;
    outcome.result["announcement"] =
        json!({"id":action.announcement.id,"topic":action.announcement.topic});
    outcome.result["effect_ids"] = json!(effects.iter().map(Effect::effect_id).collect::<Vec<_>>());
    Ok(CoordinationPlan {
        effects,
        outcome,
        next_state: announced.next_state,
    })
}
