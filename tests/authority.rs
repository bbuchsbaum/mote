use jiff::Timestamp;
use mote::{
    Store,
    authority::{self, Writer},
    ids,
    op::{ScalarSet, make_create, make_note},
    publish, reducer,
};
use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

fn command(root: &std::path::Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
    command.current_dir(root).args(args);
    command
}

#[test]
fn activation_preserves_prefix_and_admits_late_operations_after_it() {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    let issue = ids::new_bead_id();
    let create = publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            issue.clone(),
            ScalarSet {
                title: Some("order".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    Writer::acquire(&store).unwrap().enable().unwrap();
    let earlier: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
    let note = publish::publish_op(
        &store,
        &make_note(
            "alice".into(),
            issue.clone(),
            "note".into(),
            "late arrival".into(),
            earlier,
        ),
    )
    .unwrap();
    assert!(note.as_str() < create.as_str());
    assert_eq!(
        store.list_op_filenames().unwrap(),
        vec![
            format!("{create}.json", create = create.as_str()),
            format!("{note}.json", note = note.as_str())
        ]
    );
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.was_accepted(note.as_str()));
    assert_eq!(state.beads[&issue].notes.len(), 1);
    let events = mote::events::accepted_events(
        &store,
        Some(create.as_str()),
        &mote::events::EventFilter::default(),
    )
    .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event_id == note.as_str())
            .count(),
        1
    );
    let mut tailer = mote::events::EventTailer::new(&store, Some(create.as_str()), 1).unwrap();
    assert!(
        tailer
            .poll(&store, &mote::events::EventFilter::default())
            .unwrap()
            .iter()
            .any(|e| e.event_id == note.as_str())
    );
}

#[test]
fn status_is_read_only_and_activation_is_idempotent() {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    let before = authority::status(&store).unwrap();
    assert_eq!(before.schema, "mote.authority-status.v1");
    assert!(!before.enabled);
    assert_eq!(before.authority_version, 0);
    assert_eq!(before.genesis_digest, None);
    assert_eq!(
        before.capabilities,
        [
            "stable_claim_order",
            "holder_checked_handoff",
            "checked_landing_results",
        ]
    );
    let writer = Writer::acquire(&store).unwrap();
    writer.enable().unwrap();
    writer.enable().unwrap();
    drop(writer);
    let after = authority::status(&store).unwrap();
    assert!(after.enabled);
    assert_eq!(after.authority_version, 1);
    assert!(after.genesis_digest.is_some());
}

#[test]
fn cli_status_is_read_only_enable_is_idempotent_and_begin_activates() {
    let root = TempDir::new().unwrap();
    Store::init(root.path()).unwrap();
    let status = command(root.path(), &["authority", "status"])
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["schema"], "mote.authority-status.v1");
    assert_eq!(status["enabled"], false);
    assert!(status["genesis_digest"].is_null());

    let enabled = command(root.path(), &["authority", "enable"])
        .output()
        .unwrap();
    assert!(enabled.status.success(), "{enabled:?}");
    let enabled: Value = serde_json::from_slice(&enabled.stdout).unwrap();
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["activated"], true);
    let repeated = command(root.path(), &["authority", "enable"])
        .output()
        .unwrap();
    assert!(repeated.status.success(), "{repeated:?}");
    let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert!(repeated.get("activated").is_none());

    let begin_root = TempDir::new().unwrap();
    let begin_store = Store::init(begin_root.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &begin_store,
        &make_create(
            "alice".into(),
            issue.clone(),
            ScalarSet {
                title: Some("begin".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    let begin = command(
        begin_root.path(),
        &[
            "--actor",
            "alice",
            "begin",
            &issue,
            "--paths",
            "src/authority.rs",
        ],
    )
    .output()
    .unwrap();
    assert!(begin.status.success(), "{begin:?}");
    assert!(authority::status(&begin_store).unwrap().enabled);
}

#[test]
fn fresh_cli_claim_activates_before_acquisition_and_freezes_late_timestamp_order() {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    let issue = ids::new_bead_id();
    publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            issue.clone(),
            ScalarSet {
                title: Some("claim boundary".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    let claim = command(
        root.path(),
        &["--actor", "alice", "claim", &issue, "--ttl", "300"],
    )
    .output()
    .unwrap();
    assert!(claim.status.success(), "{claim:?}");
    assert!(authority::status(&store).unwrap().enabled);
    let before_late = store.list_op_filenames().unwrap();

    let earlier: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
    let late = publish::publish_op(
        &store,
        &make_note(
            "alice".into(),
            issue.clone(),
            "note".into(),
            "admitted after claim despite earlier timestamp".into(),
            earlier,
        ),
    )
    .unwrap();
    assert!(late.as_str() < before_late.last().unwrap().trim_end_matches(".json"));
    let after_late = store.list_op_filenames().unwrap();
    assert_eq!(&after_late[..before_late.len()], before_late.as_slice());
    assert_eq!(after_late.last(), Some(&format!("{}.json", late.as_str())));
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .was_accepted(late.as_str())
    );
}

#[test]
fn missing_or_modified_authority_data_fails_closed() {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    Writer::acquire(&store).unwrap().enable().unwrap();
    let genesis = authority::directory(&store).join("00000000000000000000.json");
    let original = std::fs::read(&genesis).unwrap();
    std::fs::write(&genesis, b"{}").unwrap();
    assert!(reducer::replay_store(&store).is_err());
    std::fs::write(&genesis, original).unwrap();
    assert!(reducer::replay_store(&store).is_ok());
    std::fs::remove_file(genesis).unwrap();
    assert!(reducer::replay_store(&store).is_err());
    assert!(mote::fsck::run(&store, false).is_err());
    assert!(Writer::acquire(&store).is_err());
}

#[test]
fn bypassing_publisher_cannot_inject_an_earlier_operation() {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    let issue = ids::new_bead_id();
    let name = publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            issue,
            ScalarSet {
                title: Some("bound".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    Writer::acquire(&store).unwrap().enable().unwrap();
    let path = store.ops_dir().join(format!("{}.json", name.as_str()));
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(store.ops_dir().join("late-import.json"), &bytes).unwrap();
    assert!(
        reducer::replay_store(&store)
            .unwrap_err()
            .to_string()
            .contains("unadmitted")
    );
    std::fs::remove_file(store.ops_dir().join("late-import.json")).unwrap();
    std::fs::write(path, b"{}").unwrap();
    assert!(
        reducer::replay_store(&store)
            .unwrap_err()
            .to_string()
            .contains("authority op changed")
    );
}
