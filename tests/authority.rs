use jiff::Timestamp;
use mote::{
    Store,
    authority::{self, Writer},
    ids,
    op::{ScalarSet, make_create, make_note},
    publish, reducer,
};
use tempfile::TempDir;

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
