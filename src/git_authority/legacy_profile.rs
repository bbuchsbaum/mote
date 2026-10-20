//! Closed development-only legacy reducer build profile.
use crate::{MoteError, MoteResult, canonical};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyProfile {
    pub input_digest: String,
    pub build_config_digest: String,
    pub executable_digest: String,
    pub id: String,
}

pub fn current() -> MoteResult<String> {
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
    let bytes = canonical::encode(
        &serde_json::json!({"build_config_digest":config,"executable_digest":executable_digest,"input_digest":input,"schema":"mote.legacy-profile.v1"}),
    );
    let id = blake3::hash(&[b"mote.legacy-profile.v1\0".as_slice(), bytes.as_slice()].concat())
        .to_hex()
        .to_string();
    Ok(LegacyProfile {
        input_digest: input,
        build_config_digest: config,
        executable_digest,
        id,
    }
    .id)
}
