use std::fs;
use std::process::Command;

use mote::Store;
use mote::git_authority::cache::Cache;
use mote::git_authority::history::Binding;
use serde_json::json;
use tempfile::tempdir;

#[test]
fn all_local_publication_and_projection_paths_refuse_remote_markers() {
    let td = tempdir().unwrap();
    let store = Store::init(td.path()).unwrap();
    fs::write(store.root().join("remote.json"), b"{}").unwrap();
    assert!(mote::authority::Writer::acquire(&store).is_err());
    assert!(mote::authority::Writer::acquire_for_admission(&store).is_err());
    assert!(mote::authority::Writer::acquire_discarding_foreign_format(&store).is_err());
    assert!(mote::reducer::replay_store(&store).is_err());
    assert!(
        mote::publish::publish_value(
            &store,
            json!({"kind":"create","actor":"a","entity":"test","set":{"title":"bad"}}),
            jiff::Timestamp::now()
        )
        .is_err()
    );
    for args in [
        vec!["new", "forbidden"],
        vec!["authority", "enable"],
        vec!["admit"],
        vec!["session", "start", "--as", "a"],
        vec!["set", "task", "status=doing"],
        vec!["note", "task", "--kind", "note", "bad"],
        vec!["claim", "task"],
        vec!["begin", "task"],
        vec!["done", "task"],
        vec!["board"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_mote"))
            .args(["--actor", "a"])
            .args(&args)
            .current_dir(td.path())
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_SESSION")
            .output()
            .unwrap();
        assert!(!result.status.success(), "must refuse {args:?}");
    }
    assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 0);
}

#[test]
fn incompatible_format_fails_before_creating_local_publication_state() {
    let td = tempdir().unwrap();
    let store = Store::init(td.path()).unwrap();
    let mut format: serde_json::Value =
        serde_json::from_slice(&fs::read(store.format_path()).unwrap()).unwrap();
    format["schema_version"] = json!(2);
    format["authority"] = json!({"version":4294967295u32,"genesis_hash":"git-authority-only"});
    fs::write(store.format_path(), serde_json::to_vec(&format).unwrap()).unwrap();
    assert!(mote::authority::Writer::acquire(&store).is_err());
    assert!(!store.local_dir().join("publication.lock").exists());
}

#[test]
fn cache_binding_cannot_be_replaced_and_local_state_is_private() {
    let td = tempdir().unwrap();
    let cache = Cache::open(td.path()).unwrap();
    let _lock = cache.lock().unwrap();
    let binding = Binding {
        version: 1,
        endpoint_digest: "digest".into(),
        branch_ref: "refs/heads/mote-state".into(),
        store_id: "st-test".into(),
        generation: 1,
        root: "1".repeat(40),
    };
    cache.bind(&binding).unwrap();
    cache.bind(&binding).unwrap();
    let mut other = binding;
    other.endpoint_digest = "other".into();
    assert!(cache.bind(&other).is_err());
    assert!(cache.cached_view().unwrap().is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(td.path().join("binding.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(td.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
