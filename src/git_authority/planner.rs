//! Pure, deterministic dispatch for the reviewed authority profiles.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::protocol::{
    Command, Effect, Outcome, OutcomeStatus, Request, TRANSACTION_SCHEMA, Transaction, invalid,
    valid_oid,
};
use crate::errors::MoteResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub value: String,
    pub token: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthorityState {
    pub sequence: u64,
    pub values: BTreeMap<String, Cell>,
    pub coordination: super::coordination::CoordinationState,
}

/// Compatibility name for development clients of the initial synthetic API.
pub type SyntheticState = AuthorityState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub effects: Vec<Effect>,
    pub outcome: Outcome,
    pub next_state: SyntheticState,
}

pub fn plan(state: &SyntheticState, request: &Request) -> MoteResult<Plan> {
    request.validate()?;
    let Some(sequence) = state.sequence.checked_add(1) else {
        return invalid("transaction sequence overflow");
    };
    let mut next_state = state.clone();
    next_state.sequence = sequence;
    let changes = match &request.command {
        Command::Coordination { .. } => {
            if !state.values.is_empty() {
                return invalid("coordination profile cannot contain synthetic values");
            }
            let planned = super::coordination::plan(&state.coordination, request, sequence)?;
            next_state.coordination = planned.next_state;
            return Ok(Plan {
                effects: planned.effects,
                outcome: planned.outcome,
                next_state,
            });
        }
        Command::SyntheticCas { changes } => {
            if state.coordination != super::coordination::CoordinationState::default() {
                return invalid("synthetic profile cannot contain coordination state");
            }
            changes
        }
    };
    for (index, change) in changes.iter().enumerate() {
        let actual = state.values.get(&change.key).map(|v| &v.token);
        if actual != change.expected.as_ref() {
            return Ok(Plan {
                effects: vec![],
                outcome: Outcome {
                    status: OutcomeStatus::Rejected,
                    result: json!({
                        "code": "compare_failed", "index": index, "key": change.key,
                        "expected": change.expected, "actual": actual,
                    }),
                },
                next_state,
            });
        }
    }
    let mut effects = Vec::new();
    let mut results = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        let effect_id = format!("eff-{}-{index:06}", request.request_id);
        let token = if let Some(value) = &change.value {
            next_state.values.insert(
                change.key.clone(),
                Cell {
                    value: value.clone(),
                    token: effect_id.clone(),
                },
            );
            Some(effect_id.clone())
        } else {
            next_state.values.remove(&change.key);
            None
        };
        results.push(json!({"key":change.key,"token":token}));
        effects.push(Effect::SyntheticSet {
            effect_id,
            sequence,
            index: index as u32,
            key: change.key.clone(),
            value: change.value.clone(),
        });
    }
    Ok(Plan {
        effects,
        outcome: Outcome {
            status: OutcomeStatus::Accepted,
            result: json!({"changes":results}),
        },
        next_state,
    })
}

impl Transaction {
    pub fn prepare(state: &SyntheticState, parent: &str, request: Request) -> MoteResult<Self> {
        if !valid_oid(parent) {
            return invalid("invalid transaction parent OID");
        }
        let planned = plan(state, &request)?;
        Ok(Self {
            schema: TRANSACTION_SCHEMA.into(),
            sequence: planned.next_state.sequence,
            parent: parent.into(),
            request_digest: request.digest()?,
            request,
            effects: planned.effects,
            outcome: planned.outcome,
        })
    }

    pub fn validate(&self, state: &SyntheticState, parent: &str) -> MoteResult<SyntheticState> {
        if self.schema != TRANSACTION_SCHEMA || self.parent != parent || !valid_oid(parent) {
            return invalid("transaction schema or parent mismatch");
        }
        if self.request_digest != self.request.digest()? {
            return invalid("transaction request digest mismatch");
        }
        let planned = plan(state, &self.request)?;
        if self.sequence != planned.next_state.sequence
            || self.effects != planned.effects
            || self.outcome != planned.outcome
        {
            return invalid("transaction does not reproduce its canonical request outcome");
        }
        Ok(planned.next_state)
    }
}
