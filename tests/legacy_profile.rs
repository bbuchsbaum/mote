use mote::{
    Store,
    git_authority::{legacy, legacy_profile},
};
use std::process::Command;

#[test]
fn ordinary_build_cannot_enable_a_legacy_profile_at_runtime() {
    assert!(
        option_env!("MOTE_LEGACY_INPUT_DIGEST")
            .unwrap_or_default()
            .is_empty(),
        "run ordinary-build qualification without a development build receipt"
    );
    assert!(
        legacy_profile::current()
            .unwrap_err()
            .to_string()
            .contains("profile unavailable")
    );
    let td = tempfile::tempdir().unwrap();
    let store = Store::init(td.path()).unwrap();
    let before = std::fs::read(store.format_path()).unwrap();
    assert!(legacy::capture(&store, None).is_err());
    assert!(!store.local_dir().join("publication.lock").exists());
    assert_eq!(std::fs::read(store.format_path()).unwrap(), before);
    let out = Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(td.path())
        .env(
            "MOTE_LEGACY_BUILD_RECEIPT",
            "runtime injection cannot change embedded evidence",
        )
        .args(["--json", "authority", "legacy-profile"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("profile unavailable"));
}
