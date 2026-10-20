//! Versioned, strict wire format for the experimental Git authority.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

use crate::canonical;
use crate::errors::{MoteError, MoteResult};

pub const MANIFEST_SCHEMA: &str = "mote.git-authority.v1";
pub const TRANSACTION_SCHEMA: &str = "mote.git-transaction.v1";
pub const CAPABILITIES: [&str; 3] = [
    "canonical-transactions-v1",
    "explicit-release-v1",
    "synthetic-cas-v1",
];
pub const COORDINATION_CAPABILITIES: [&str; 3] = [
    "canonical-transactions-v1",
    "coordination-core-v1",
    "explicit-release-v1",
];
pub const LIFECYCLE_CAPABILITIES: [&str; 4] = [
    "canonical-transactions-v1",
    "coordination-core-v1",
    "coordination-lifecycle-v1",
    "explicit-release-v1",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub store_id: String,
    pub generation: u64,
    pub ownership: String,
    pub capabilities: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    pub legacy_digest: Option<String>,
}

impl Manifest {
    pub fn new(store_id: String) -> Self {
        Self {
            schema: MANIFEST_SCHEMA.into(),
            store_id,
            generation: 1,
            ownership: "explicit-release-v1".into(),
            capabilities: CAPABILITIES.iter().map(|x| (*x).into()).collect(),
            legacy_digest: None,
        }
    }

    /// Development-only coordination core; never a synthetic profile upgrade.
    pub fn coordination(store_id: String) -> Self {
        let mut manifest = Self::new(store_id);
        manifest.capabilities = COORDINATION_CAPABILITIES
            .iter()
            .map(|x| (*x).into())
            .collect();
        manifest
    }

    pub fn is_coordination(&self) -> bool {
        self.capabilities == COORDINATION_CAPABILITIES
            || self.capabilities == LIFECYCLE_CAPABILITIES
    }

    pub fn lifecycle(store_id: String) -> Self {
        let mut manifest = Self::coordination(store_id);
        manifest.capabilities = LIFECYCLE_CAPABILITIES.iter().map(|x| (*x).into()).collect();
        manifest
    }

    pub fn allows(&self, command: &Command) -> MoteResult<()> {
        self.validate()?;
        match (self.is_coordination(), command) {
            (false, Command::SyntheticCas { .. }) => Ok(()),
            (true, Command::Coordination { action })
                if !action.is_lifecycle() || self.capabilities == LIFECYCLE_CAPABILITIES =>
            {
                Ok(())
            }
            _ => invalid("command is not supported by the immutable authority profile"),
        }
    }

    pub fn validate(&self) -> MoteResult<()> {
        if self.schema != MANIFEST_SCHEMA {
            return invalid("unsupported Git authority schema");
        }
        let suffix = self.store_id.strip_prefix("st-").unwrap_or("");
        if suffix.is_empty()
            || suffix.len() > 128
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return invalid("invalid authority store ID");
        }
        if self.generation == 0 || self.ownership != "explicit-release-v1" {
            return invalid("unsupported authority generation or ownership policy");
        }
        if self.capabilities != CAPABILITIES
            && self.capabilities != COORDINATION_CAPABILITIES
            && self.capabilities != LIFECYCLE_CAPABILITIES
        {
            return invalid("unsupported or noncanonical Git authority capabilities");
        }
        if self.legacy_digest.is_some() {
            return invalid("migrated roots require a reviewed legacy profile; not supported yet");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub request_id: String,
    pub actor: String,
    pub session: String,
    pub timestamp: String,
    pub command: Command,
}

impl Request {
    pub fn validate(&self) -> MoteResult<()> {
        if self.version != 1 {
            return invalid("unsupported request version");
        }
        if !simple_identifier(&self.request_id, 100) {
            return invalid(
                "request ID must contain 1-100 ASCII letters, digits, hyphens or underscores",
            );
        }
        for value in [&self.actor, &self.session] {
            if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                return invalid("actor and session must be nonempty bounded identifiers");
            }
        }
        self.timestamp
            .parse::<jiff::Timestamp>()
            .map_err(|_| MoteError::Invalid("invalid request timestamp".into()))?;
        match &self.command {
            Command::Coordination { action } => super::coordination::validate_action(action)?,
            Command::SyntheticCas { changes } => {
                if changes.is_empty() || changes.len() > 64 {
                    return invalid("synthetic CAS requires 1-64 changes");
                }
                let mut keys = BTreeSet::new();
                for change in changes {
                    if !simple_identifier(&change.key, 128) || !keys.insert(&change.key) {
                        return invalid("synthetic CAS keys must be distinct ASCII identifiers");
                    }
                }
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> MoteResult<String> {
        self.validate()?;
        Ok(blake3::hash(&encode(self)?).to_hex().to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "synthetic.cas")]
    SyntheticCas { changes: Vec<Change> },
    #[serde(rename = "coordination")]
    Coordination { action: super::coordination::Action },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Change {
    pub key: String,
    #[serde(deserialize_with = "required_option")]
    pub expected: Option<String>,
    #[serde(deserialize_with = "required_option")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    SyntheticSet {
        effect_id: String,
        sequence: u64,
        index: u32,
        key: String,
        #[serde(deserialize_with = "required_option")]
        value: Option<String>,
    },
    TaskOp {
        effect_id: String,
        sequence: u64,
        index: u32,
        op: Value,
    },
    Ownership {
        effect_id: String,
        sequence: u64,
        index: u32,
        transition: Box<super::coordination::Transition>,
    },
}

impl Effect {
    pub fn effect_id(&self) -> &str {
        match self {
            Self::SyntheticSet { effect_id, .. }
            | Self::TaskOp { effect_id, .. }
            | Self::Ownership { effect_id, .. } => effect_id,
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Self::SyntheticSet { sequence, .. }
            | Self::TaskOp { sequence, .. }
            | Self::Ownership { sequence, .. } => *sequence,
        }
    }

    pub fn index(&self) -> u32 {
        match self {
            Self::SyntheticSet { index, .. }
            | Self::TaskOp { index, .. }
            | Self::Ownership { index, .. } => *index,
        }
    }

    /// An ordered adapter ID for legacy consumers that compare op IDs.
    pub fn adapter_id(&self) -> String {
        format!(
            "gaop-{:020}-{:06}-{}",
            self.sequence(),
            self.index(),
            self.effect_id()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutcomeStatus {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub status: OutcomeStatus,
    pub result: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transaction {
    pub schema: String,
    pub sequence: u64,
    pub parent: String,
    pub request: Request,
    pub request_digest: String,
    pub effects: Vec<Effect>,
    pub outcome: Outcome,
}

pub fn transaction_path(sequence: u64) -> String {
    format!("transactions/{sequence:020}.json")
}

pub fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn simple_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub fn encode<T: Serialize>(value: &T) -> MoteResult<Vec<u8>> {
    Ok(canonical::encode(&serde_json::to_value(value)?))
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// Reject duplicate keys and floating-point syntax before typed decoding.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> MoteResult<T> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue::deserialize(&mut de)?;
    de.end()?;
    Ok(serde_json::from_value(value.0)?)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with unique object keys and integer numbers")
            }
            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(v))))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(Number::from(v))))
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Err(E::custom("floating-point JSON is not permitted"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v.into())))
            }
            fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v)))
            }
            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = seq.next_element::<StrictValue>()? {
                    values.push(value.0);
                }
                Ok(StrictValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut values = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(serde::de::Error::custom("duplicate JSON object key"));
                    }
                    values.insert(key, map.next_value::<StrictValue>()?.0);
                }
                Ok(StrictValue(Value::Object(values)))
            }
        }
        de.deserialize_any(StrictVisitor)
    }
}

pub(crate) fn invalid<T>(message: &str) -> MoteResult<T> {
    Err(MoteError::Invalid(message.into()))
}
