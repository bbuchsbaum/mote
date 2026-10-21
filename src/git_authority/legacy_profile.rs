//! Portable replay compatibility and separate exact-executor provenance.
use crate::{MoteError, MoteResult, canonical};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LegacyProfile {
    pub input_digest: String,
    pub build_config_digest: String,
    pub executable_digest: String,
    pub id: String,
    pub portable_id: String,
}

/// Original v1 identity. Keep this exact-executable pin for existing archives
/// and interrupted rollback journals; never reinterpret it as portable.
pub fn current() -> MoteResult<String> {
    Ok(evidence()?.id)
}

/// Preferred identity for new archives: the closed source/dependency inputs
/// match, while the target and actual executable may differ.
pub fn portable() -> MoteResult<String> {
    Ok(evidence()?.portable_id)
}

pub fn require_supported(profile: &str) -> MoteResult<()> {
    let build = evidence()?;
    if supports(&build, profile) {
        Ok(())
    } else {
        Err(MoteError::Invalid(
            "unsupported legacy replay profile; use a matching portable closed build, or the original retained executable for an exact v1 archive".into(),
        ))
    }
}

fn supports(build: &LegacyProfile, profile: &str) -> bool {
    // No failed exact-v1 lookup falls back to portable v2.
    if profile.starts_with("portable-v2:") {
        profile == build.portable_id
    } else {
        profile == build.id
    }
}

pub fn evidence() -> MoteResult<LegacyProfile> {
    let input = option_env!("MOTE_LEGACY_INPUT_DIGEST")
        .unwrap_or_default()
        .to_owned();
    let config = option_env!("MOTE_LEGACY_BUILD_CONFIG_DIGEST")
        .unwrap_or_default()
        .to_owned();
    if input.is_empty() || config.is_empty() {
        return Err(MoteError::Invalid(
            "legacy profile unavailable: this binary lacks a validated closed build receipt".into(),
        ));
    }
    let exe = std::env::current_exe().map_err(MoteError::Io)?;
    let executable_digest = blake3::hash(&std::fs::read(exe).map_err(MoteError::Io)?)
        .to_hex()
        .to_string();
    Ok(identities(input, config, executable_digest))
}

fn identities(input: String, config: String, executable_digest: String) -> LegacyProfile {
    let bytes = canonical::encode(
        &serde_json::json!({"build_config_digest":config,"executable_digest":executable_digest,"input_digest":input,"schema":"mote.legacy-profile.v1"}),
    );
    let id = blake3::hash(&[b"mote.legacy-profile.v1\0".as_slice(), bytes.as_slice()].concat())
        .to_hex()
        .to_string();
    let portable_bytes = canonical::encode(&serde_json::json!({
        "input_digest":input,"schema":"mote.legacy-portable-profile.v2"
    }));
    let portable_digest = blake3::hash(
        &[
            b"mote.legacy-portable-profile.v2\0".as_slice(),
            portable_bytes.as_slice(),
        ]
        .concat(),
    );
    LegacyProfile {
        input_digest: input,
        build_config_digest: config,
        executable_digest,
        id,
        portable_id: format!("portable-v2:{}", portable_digest.to_hex()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_identity_separates_source_compatibility_from_build_provenance() {
        let a = identities(
            "source-and-lock".into(),
            "mac-arm-build".into(),
            "exe-a".into(),
        );
        let b = identities(
            "source-and-lock".into(),
            "mac-intel-build".into(),
            "exe-b".into(),
        );
        assert_eq!(a.portable_id, b.portable_id);
        assert_ne!(a.id, b.id);
        assert!(supports(&b, &a.portable_id));
        assert!(!supports(&b, &a.id));
        assert!(supports(&a, &a.id));
        let changed = identities(
            "changed-helper-or-lock".into(),
            a.build_config_digest.clone(),
            a.executable_digest.clone(),
        );
        assert_ne!(a.portable_id, changed.portable_id);
        assert!(!supports(&changed, &a.portable_id));
        assert!(!supports(&a, "portable-v3:unknown"));
        assert!(!supports(&a, "unknown"));
    }
}
