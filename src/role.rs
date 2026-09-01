//! First-class operational role value types.
//!
//! A role definition is immutable policy. Assignments are explicit TTL grants
//! bound to one actor session; reducer replay derives their disposition from an
//! injected timestamp and never infers a role from presence or activity.

use serde::{Deserialize, Serialize};

pub const ROLE_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleExclusionCode {
    ConcurrentRole,
    CandidateProposer,
    CandidateAuthorizer,
    CandidateNamedReviewer,
    CandidateEvidenceProducer,
    CandidatePathReservation,
}

impl RoleExclusionCode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "concurrent_role" => Some(Self::ConcurrentRole),
            "candidate_proposer" => Some(Self::CandidateProposer),
            "candidate_authorizer" => Some(Self::CandidateAuthorizer),
            "candidate_named_reviewer" => Some(Self::CandidateNamedReviewer),
            "candidate_evidence_producer" => Some(Self::CandidateEvidenceProducer),
            "candidate_path_reservation" => Some(Self::CandidatePathReservation),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConcurrentRole => "concurrent_role",
            Self::CandidateProposer => "candidate_proposer",
            Self::CandidateAuthorizer => "candidate_authorizer",
            Self::CandidateNamedReviewer => "candidate_named_reviewer",
            Self::CandidateEvidenceProducer => "candidate_evidence_producer",
            Self::CandidatePathReservation => "candidate_path_reservation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleExclusion {
    pub code: RoleExclusionCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_role_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RoleAssignmentClock {
    pub assignment_id: String,
    pub clock_op_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleAssignmentDisposition {
    NotStarted,
    Active,
    Released,
    Expired,
    SessionEnded,
    SessionMissing,
}

impl RoleAssignmentDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Active => "active",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::SessionEnded => "session_ended",
            Self::SessionMissing => "session_missing",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleDemandSource {
    pub kind: String,
    pub source_id: String,
    pub required: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleCoverage {
    pub as_of_ts: String,
    pub active_assignment_ids: Vec<String>,
    pub active_count: u32,
    pub capacity: u32,
    pub minimum_active: u32,
    pub demanded_count: u32,
    pub available_capacity: u32,
    pub coverage_shortfall: u32,
    pub vacant: bool,
    pub demand_sources: Vec<RoleDemandSource>,
}

pub fn valid_role_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let bytes = name.as_bytes();
    let is_alnum = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    is_alnum(bytes[0])
        && is_alnum(bytes[bytes.len() - 1])
        && bytes.iter().all(|byte| is_alnum(*byte) || *byte == b'-')
}
