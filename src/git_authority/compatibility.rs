//! Actual local binary evidence and immutable authority-profile compatibility.

use std::path::PathBuf;

use serde::Serialize;

use super::protocol;
use crate::{MoteError, MoteResult};

const BINARY_SCHEMA: &str = "mote.git-binary.v1";
const OWNERSHIP: &str = "explicit-release-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BinaryEvidence {
    pub schema: String,
    pub package_version: String,
    pub executable: ExecutableEvidence,
    pub build: BuildEvidence,
    pub supported_profiles: Vec<SupportedProfile>,
    pub ownership: String,
    pub legacy_replay: LegacyReplayEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecutableEvidence {
    pub path: PathBuf,
    pub algorithm: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildEvidence {
    pub source_sha: Option<String>,
    pub dirty: Option<bool>,
    pub target: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SupportedProfile {
    pub name: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyReplayEvidence {
    pub status: String,
    pub profile_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompatibilityEvidence {
    pub required_capabilities: Vec<String>,
    pub structural_supported: bool,
    pub supported_profile: Option<String>,
    pub reason: Option<String>,
    pub remediation: Option<String>,
    pub legacy_replay_requirement: String,
}

/// Returns evidence from the currently executing binary, without inferring build provenance.
pub fn binary_evidence() -> MoteResult<BinaryEvidence> {
    let path = std::env::current_exe().map_err(MoteError::Io)?;
    let digest = blake3::hash(&std::fs::read(&path).map_err(MoteError::Io)?)
        .to_hex()
        .to_string();
    Ok(BinaryEvidence {
        schema: BINARY_SCHEMA.into(),
        package_version: env!("CARGO_PKG_VERSION").into(),
        executable: ExecutableEvidence {
            path,
            algorithm: "blake3".into(),
            digest,
        },
        build: BuildEvidence {
            source_sha: None,
            dirty: None,
            target: None,
            status: "unavailable_f10".into(),
        },
        supported_profiles: supported_profiles(),
        ownership: OWNERSHIP.into(),
        legacy_replay: legacy_replay(),
    })
}

/// Reports whether a manifest is structurally one of this binary's immutable profiles.
pub fn compare_manifest(manifest: &protocol::Manifest) -> CompatibilityEvidence {
    let required_capabilities = manifest.capabilities.clone();
    let legacy_replay_requirement = if manifest.legacy_digest.is_some() {
        "requires_verified_archive"
    } else {
        "not_required"
    }
    .into();
    if let Err(error) = manifest.validate() {
        return CompatibilityEvidence {
            required_capabilities,
            structural_supported: false,
            supported_profile: None,
            reason: Some(error.to_string()),
            remediation: Some(
                "use an exact supported immutable profile; do not downgrade or recreate the authority"
                    .into(),
            ),
            legacy_replay_requirement,
        };
    }

    let supported_profile = supported_profiles()
        .into_iter()
        .find(|profile| profile.capabilities == required_capabilities)
        .map(|profile| profile.name);
    let structural_supported = supported_profile.is_some();
    CompatibilityEvidence {
        required_capabilities,
        structural_supported,
        supported_profile,
        reason: (!structural_supported)
            .then_some("manifest capabilities are not supported by this binary".into()),
        remediation: (!structural_supported).then_some(
            "use an exact supported immutable profile; do not downgrade or recreate the authority"
                .into(),
        ),
        legacy_replay_requirement,
    }
}

fn supported_profiles() -> Vec<SupportedProfile> {
    [
        ("synthetic", protocol::CAPABILITIES.as_slice()),
        (
            "coordination",
            protocol::COORDINATION_CAPABILITIES.as_slice(),
        ),
        ("lifecycle", protocol::LIFECYCLE_CAPABILITIES.as_slice()),
        (
            "migrated-root",
            protocol::MIGRATED_ROOT_CAPABILITIES.as_slice(),
        ),
        (
            "recoverable-migrated",
            protocol::RECOVERABLE_MIGRATED_CAPABILITIES.as_slice(),
        ),
        (
            "communication",
            protocol::COMMUNICATION_CAPABILITIES.as_slice(),
        ),
        (
            "migrated-communication",
            protocol::MIGRATED_COMMUNICATION_CAPABILITIES.as_slice(),
        ),
    ]
    .into_iter()
    .map(|(name, capabilities)| SupportedProfile {
        name: name.into(),
        capabilities: capabilities
            .iter()
            .map(|capability| (*capability).into())
            .collect(),
    })
    .collect()
}

fn legacy_replay() -> LegacyReplayEvidence {
    legacy_replay_from(super::legacy_profile::current())
}

fn legacy_replay_from(result: MoteResult<String>) -> LegacyReplayEvidence {
    match result {
        Ok(profile_id) => LegacyReplayEvidence {
            status: "available".into(),
            profile_id: Some(profile_id),
            reason: None,
        },
        Err(error) => LegacyReplayEvidence {
            status: "unavailable".into(),
            profile_id: None,
            reason: Some(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> protocol::Manifest {
        protocol::Manifest::new("st-compatible".into())
    }

    #[test]
    fn binary_metadata_marks_build_provenance_unknown_and_hashes_the_executable() {
        let evidence = binary_evidence().unwrap();
        assert_eq!(evidence.schema, BINARY_SCHEMA);
        assert_eq!(evidence.package_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(evidence.executable.algorithm, "blake3");
        assert_eq!(evidence.executable.digest.len(), 64);
        assert!(
            evidence
                .executable
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(evidence.build.status, "unavailable_f10");
        assert_eq!(evidence.build.source_sha, None);
        assert_eq!(evidence.build.dirty, None);
        assert_eq!(evidence.build.target, None);
        assert_eq!(evidence.ownership, OWNERSHIP);
    }

    #[test]
    fn every_exact_profile_is_structurally_supported() {
        for profile in supported_profiles() {
            let mut value = manifest();
            value.capabilities = profile.capabilities.clone();
            if value
                .capabilities
                .iter()
                .any(|capability| capability == "legacy-packed-v1")
            {
                value.legacy_digest = Some("a".repeat(64));
            }
            let evidence = compare_manifest(&value);
            assert!(evidence.structural_supported, "{}", profile.name);
            assert_eq!(
                evidence.supported_profile.as_deref(),
                Some(profile.name.as_str())
            );
            assert_eq!(evidence.reason, None);
        }
    }

    #[test]
    fn malformed_profiles_are_not_structurally_supported() {
        let mut unknown_capability = manifest();
        unknown_capability.capabilities.push("unknown-v1".into());
        assert!(!compare_manifest(&unknown_capability).structural_supported);

        let mut noncanonical_order = manifest();
        noncanonical_order.capabilities.swap(0, 1);
        assert!(!compare_manifest(&noncanonical_order).structural_supported);

        let mut wrong_ownership = manifest();
        wrong_ownership.ownership = "implicit-release-v1".into();
        assert!(!compare_manifest(&wrong_ownership).structural_supported);

        let mut wrong_generation = manifest();
        wrong_generation.generation = 0;
        assert!(!compare_manifest(&wrong_generation).structural_supported);
    }

    #[test]
    fn migrated_manifest_requires_a_verified_archive_for_replay() {
        let value = protocol::Manifest::migrated("st-compatible".into(), "a".repeat(64));
        let evidence = compare_manifest(&value);
        assert!(evidence.structural_supported);
        assert_eq!(
            evidence.legacy_replay_requirement,
            "requires_verified_archive"
        );
    }

    #[test]
    fn unavailable_legacy_profile_is_ordinary_binary_metadata() {
        let evidence = legacy_replay_from(Err(MoteError::Invalid(
            "legacy profile unavailable: no closed build receipt".into(),
        )));
        assert_eq!(evidence.status, "unavailable");
        assert_eq!(evidence.profile_id, None);
        assert!(evidence.reason.unwrap().contains("closed build receipt"));
    }
}
