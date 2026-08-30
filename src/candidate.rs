//! Candidate protocol value types and Git evidence probes.
//!
//! Reducer replay consumes only serialized receipts. Git is consulted here by
//! explicit CLI operations before publication, never while materializing state.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::errors::{MoteError, MoteResult};

pub const CANDIDATE_PROTOCOL_VERSION: u32 = 1;
pub const GIT_ANCESTRY_EVIDENCE: &str = "git-ancestry";
pub const GIT_LANDING_EVIDENCE: &str = "git-landing";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidatePhase {
    Pending,
    Superseded,
    Abandoned,
    Landed,
}

impl CandidatePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Superseded => "superseded",
            Self::Abandoned => "abandoned",
            Self::Landed => "landed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Approve,
    Block,
    Comment,
}

impl ReviewVerdict {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "approve" => Some(Self::Approve),
            "block" => Some(Self::Block),
            "comment" => Some(Self::Comment),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Block => "block",
            Self::Comment => "comment",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    Pass,
    Fail,
    Unavailable,
    Ambiguous,
}

impl EvidenceOutcome {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "unavailable" => Some(Self::Unavailable),
            "ambiguous" => Some(Self::Ambiguous),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unavailable => "unavailable",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationStatus {
    Granted,
    Conditional,
    Revoked,
    Consumed,
}

impl AuthorizationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Conditional => "conditional",
            Self::Revoked => "revoked",
            Self::Consumed => "consumed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRelationKind {
    Ancestor,
    NotAncestor,
    Unavailable,
    Ambiguous,
}

impl GitRelationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ancestor => "ancestor",
            Self::NotAncestor => "not_ancestor",
            Self::Unavailable => "unavailable",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRequirement {
    pub name: String,
    pub kind: String,
    pub producers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCandidateRelation {
    pub candidate_id: String,
    pub proposal_op_id: String,
    pub commit_oid: String,
    /// Relation of the known candidate commit to this candidate's immutable base.
    /// Missing on legacy receipts, which consumers must treat as ambiguous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_relation: Option<GitRelationKind>,
    /// Relation of the known candidate commit to this candidate's tip.
    pub relation: GitRelationKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitAncestryReceipt {
    pub repository_id: String,
    pub object_format: String,
    pub common_dir_hash: String,
    pub commit_oid: String,
    pub base_oid: String,
    pub parent_oids: Vec<String>,
    pub base_is_ancestor: Option<bool>,
    pub candidate_relations: Vec<GitCandidateRelation>,
    pub covered_candidates: Vec<(String, String)>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLandingReceipt {
    pub repository_id: String,
    pub object_format: String,
    pub candidate_oid: String,
    pub target_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_tip: Option<String>,
    pub after_tip: String,
    pub candidate_reachable: Option<bool>,
    pub authorization_op_id: String,
    pub basis_op_ids: Vec<String>,
    pub git_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateEvidencePayload {
    GitAncestry(GitAncestryReceipt),
    GitLanding(GitLandingReceipt),
    External {
        digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownCandidate {
    pub candidate_id: String,
    pub proposal_op_id: String,
    pub repository_id: String,
    pub commit_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LandabilityReason {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Landability {
    pub landable: bool,
    pub reason_codes: Vec<String>,
    pub reasons: Vec<LandabilityReason>,
}

impl Landability {
    pub fn from_reasons(mut reasons: Vec<LandabilityReason>) -> Self {
        reasons.sort_by(|a, b| {
            a.code
                .cmp(&b.code)
                .then_with(|| a.subject.cmp(&b.subject))
                .then_with(|| a.detail.cmp(&b.detail))
        });
        let mut reason_codes: Vec<String> = reasons.iter().map(|r| r.code.clone()).collect();
        reason_codes.sort();
        reason_codes.dedup();
        Self {
            landable: reasons.is_empty(),
            reason_codes,
            reasons,
        }
    }
}

pub fn evidence_id<T: Serialize>(value: &T) -> MoteResult<String> {
    let json = serde_json::to_value(value)?;
    Ok(format!(
        "evid-{}",
        blake3::hash(&canonical::encode(&json)).to_hex()
    ))
}

pub fn action_digest<T: Serialize>(value: &T) -> MoteResult<String> {
    let mut json = serde_json::to_value(value)?;
    if let Some(object) = json.as_object_mut() {
        object.remove("v");
        object.remove("op");
        object.remove("ts");
        object.remove("idempotency_key");
    }
    Ok(blake3::hash(&canonical::encode(&json)).to_hex().to_string())
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("git {} failed to start: {error}", args.join(" ")))
}

fn git_text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_output(cwd, args)?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|error| format!("git {} returned non-UTF-8 output: {error}", args.join(" ")))
}

fn canonical_common_dir(cwd: &Path) -> Result<PathBuf, String> {
    let raw = git_text(cwd, &["rev-parse", "--git-common-dir"])?;
    let path = PathBuf::from(raw);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    std::fs::canonicalize(&path).map_err(|error| {
        format!(
            "cannot canonicalize git common dir {}: {error}",
            path.display()
        )
    })
}

fn repository_identity(cwd: &Path) -> Result<(String, String, String), String> {
    let object_format = git_text(cwd, &["rev-parse", "--show-object-format"])?;
    if object_format != "sha1" && object_format != "sha256" {
        return Err(format!("unsupported Git object format {object_format}"));
    }
    let common_dir = canonical_common_dir(cwd)?;
    let common_dir_hash = blake3::hash(common_dir.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    let mut identity = Vec::new();
    identity.extend_from_slice(object_format.as_bytes());
    identity.push(0);
    identity.extend_from_slice(common_dir_hash.as_bytes());
    let repository_id = format!("repo-{}", blake3::hash(&identity).to_hex());
    Ok((repository_id, object_format, common_dir_hash))
}

fn resolve_commit(cwd: &Path, reference: &str) -> Result<String, String> {
    git_text(
        cwd,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
}

fn commit_parents(cwd: &Path, commit: &str) -> Result<Vec<String>, String> {
    let line = git_text(cwd, &["rev-list", "--parents", "-n", "1", commit])?;
    let mut ids = line.split_whitespace();
    let observed = ids
        .next()
        .ok_or_else(|| "git rev-list returned no commit".to_string())?;
    if observed != commit {
        return Err(format!(
            "git rev-list returned {observed}, expected {commit}"
        ));
    }
    Ok(ids.map(str::to_string).collect())
}

fn ancestor_relation(cwd: &Path, ancestor: &str, descendant: &str) -> GitRelationKind {
    match git_output(cwd, &["merge-base", "--is-ancestor", ancestor, descendant]) {
        Ok(output) if output.status.code() == Some(0) => GitRelationKind::Ancestor,
        Ok(output) if output.status.code() == Some(1) => GitRelationKind::NotAncestor,
        Ok(_) => GitRelationKind::Ambiguous,
        Err(_) => GitRelationKind::Unavailable,
    }
}

fn git_version(cwd: &Path) -> String {
    git_text(cwd, &["--version"]).unwrap_or_else(|error| format!("unavailable: {error}"))
}

pub fn probe_ancestry(
    cwd: &Path,
    commit_ref: &str,
    base_ref: &str,
    known: &[KnownCandidate],
) -> Result<GitAncestryReceipt, String> {
    let (repository_id, object_format, common_dir_hash) = repository_identity(cwd)?;
    let commit_oid = resolve_commit(cwd, commit_ref)?;
    let base_oid = resolve_commit(cwd, base_ref)?;
    let parent_oids = commit_parents(cwd, &commit_oid)?;
    let base_relation = ancestor_relation(cwd, &base_oid, &commit_oid);
    let base_is_ancestor = match base_relation {
        GitRelationKind::Ancestor => Some(true),
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };

    let mut candidate_relations = Vec::new();
    let mut covered_candidates = Vec::new();
    for candidate in known {
        if candidate.repository_id != repository_id {
            continue;
        }
        candidate_relations.push(GitCandidateRelation {
            candidate_id: candidate.candidate_id.clone(),
            proposal_op_id: candidate.proposal_op_id.clone(),
            commit_oid: candidate.commit_oid.clone(),
            base_relation: Some(ancestor_relation(cwd, &candidate.commit_oid, &base_oid)),
            relation: ancestor_relation(cwd, &candidate.commit_oid, &commit_oid),
        });
        covered_candidates.push((
            candidate.candidate_id.clone(),
            candidate.proposal_op_id.clone(),
        ));
    }
    candidate_relations.sort_by(|a, b| a.candidate_id.cmp(&b.candidate_id));
    covered_candidates.sort();

    Ok(GitAncestryReceipt {
        repository_id,
        object_format,
        common_dir_hash,
        commit_oid,
        base_oid,
        parent_oids,
        base_is_ancestor,
        candidate_relations,
        covered_candidates,
        git_version: git_version(cwd),
        detail: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn probe_landing(
    cwd: &Path,
    repository_id: &str,
    object_format: &str,
    candidate_oid: &str,
    target_ref: &str,
    before_tip: Option<&str>,
    authorization_op_id: &str,
    basis_op_ids: Vec<String>,
) -> Result<GitLandingReceipt, String> {
    let (observed_repository, observed_format, _) = repository_identity(cwd)?;
    if observed_repository != repository_id || observed_format != object_format {
        return Err("landing repository identity does not match candidate".into());
    }
    let after_tip = resolve_commit(cwd, target_ref)?;
    let before_tip = before_tip
        .map(|reference| resolve_commit(cwd, reference))
        .transpose()?;
    let relation = ancestor_relation(cwd, candidate_oid, &after_tip);
    let candidate_reachable = match relation {
        GitRelationKind::Ancestor => Some(true),
        GitRelationKind::NotAncestor => Some(false),
        GitRelationKind::Unavailable | GitRelationKind::Ambiguous => None,
    };
    Ok(GitLandingReceipt {
        repository_id: observed_repository,
        object_format: observed_format,
        candidate_oid: candidate_oid.to_string(),
        target_ref: target_ref.to_string(),
        before_tip,
        after_tip,
        candidate_reachable,
        authorization_op_id: authorization_op_id.to_string(),
        basis_op_ids,
        git_version: git_version(cwd),
        detail: None,
    })
}

pub fn validate_full_oid(object_format: &str, oid: &str) -> bool {
    let expected = match object_format {
        "sha1" => 40,
        "sha256" => 64,
        _ => return false,
    };
    oid.len() == expected
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn git_probe_error(error: String) -> MoteError {
    MoteError::Other(format!("Git evidence unavailable: {error}"))
}
