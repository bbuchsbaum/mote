//! Authority is per replica while the op log is replicated through Git: ops
//! pulled from another clone stay unadmitted until `mote admit`, and activating
//! authority never changes a tracked file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use jiff::Timestamp;
use mote::{
    Store,
    authority::{self, BindingLocation, Writer},
    ids,
    op::{ScalarSet, make_create},
    publish, reducer,
};
use serde_json::Value;
use tempfile::TempDir;

fn mote(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .args(args)
        .current_dir(dir)
        .env_remove("MOTE_ACTOR")
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_SESSION")
        .output()
        .unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn fails(out: &Output) -> String {
    assert!(
        !out.status.success(),
        "expected failure\nstdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

const IGNORE: &str = ".mote/local/\n.mote/tmp/\n.mote/authority/\n";

/// A bare origin plus two clones sharing one Git-versioned op log. The first
/// clone commits the store with one bead; the second clones it.
fn two_replicas() -> (TempDir, PathBuf, PathBuf, String) {
    let td = TempDir::new().unwrap();
    let origin = td.path().join("origin.git");
    git(
        td.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    let a = td.path().join("a");
    std::fs::create_dir(&a).unwrap();
    git(&a, &["init", "-q", "-b", "main"]);
    ok(&mote(&a, &["init"]));
    std::fs::write(a.join(".gitignore"), IGNORE).unwrap();
    let id = ok(&mote(&a, &["--actor", "alice", "new", "shared"]))
        .trim()
        .to_string();
    git(&a, &["add", ".gitignore", ".mote"]);
    git(&a, &["commit", "-q", "-m", "store"]);
    git(&a, &["remote", "add", "origin", origin.to_str().unwrap()]);
    git(&a, &["push", "-q", "origin", "main"]);
    let b = td.path().join("b");
    git(
        td.path(),
        &["clone", "-q", "-b", "main", origin.to_str().unwrap(), "b"],
    );
    (td, a, b, id)
}

fn publish_ops_and_sync(dir: &Path) {
    git(dir, &["add", ".mote/ops"]);
    git(dir, &["commit", "-q", "-m", "ops"]);
    git(
        dir,
        &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"],
    );
    git(dir, &["push", "-q", "origin", "main"]);
}

/// `git status` limited to tracked files: empty when nothing tracked changed.
fn tracked_changes(dir: &Path) -> String {
    git(dir, &["status", "--porcelain", "--untracked-files=no"])
}

/// Board JSON without its evaluation clock, which differs between calls.
fn board(dir: &Path) -> Value {
    let mut board: Value =
        serde_json::from_str(&ok(&mote(dir, &["--actor", "observer", "--json", "board"]))).unwrap();
    strip_clock(&mut board);
    board
}

fn strip_clock(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !matches!(key.as_str(), "now" | "as_of" | "as_of_ts"));
            map.values_mut().for_each(strip_clock);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_clock),
        _ => {}
    }
}

#[test]
fn two_replicas_exchange_fenced_ops_through_git_and_converge_after_admit() {
    let (_td, a, b, shared) = two_replicas();

    // Each replica activates its own authority through begin, on its own bead.
    let a_work = ok(&mote(&a, &["--actor", "alice", "new", "a work"]))
        .trim()
        .to_string();
    let begin = mote(
        &a,
        &["--actor", "alice", "begin", &a_work, "--paths", "src/a"],
    );
    ok(&begin);
    let note = String::from_utf8_lossy(&begin.stderr);
    assert!(
        note.contains("Git-versioned") && note.contains("mote admit"),
        "{note}"
    );
    let b_work = ok(&mote(&b, &["--actor", "bob", "new", "b work"]))
        .trim()
        .to_string();
    ok(&mote(
        &b,
        &["--actor", "bob", "begin", &b_work, "--paths", "src/b"],
    ));
    ok(&mote(
        &b,
        &[
            "--actor", "bob", "note", &shared, "--kind", "note", "from b",
        ],
    ));
    for dir in [&a, &b] {
        assert!(authority::enabled(
            &Store::open(&dir.join(".mote")).unwrap()
        ));
        assert_eq!(
            tracked_changes(dir),
            "",
            "activation changed a tracked file"
        );
    }

    publish_ops_and_sync(&a);
    publish_ops_and_sync(&b);
    git(
        &a,
        &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"],
    );

    // Pulled ops are unadmitted: reads fail closed and name the remedy.
    for dir in [&a, &b] {
        let error = fails(&mote(dir, &["--actor", "observer", "ls"]));
        assert!(error.contains("unadmitted operation"), "{error}");
        assert!(error.contains("mote admit"), "{error}");
    }

    let a_admit: Value = serde_json::from_str(&ok(&mote(&a, &["--json", "admit"]))).unwrap();
    assert_eq!(a_admit["schema"], "mote.admit.v1");
    assert!(!a_admit["admitted"].as_array().unwrap().is_empty());
    let b_admit: Value = serde_json::from_str(&ok(&mote(&b, &["--json", "admit"]))).unwrap();
    assert!(!b_admit["admitted"].as_array().unwrap().is_empty());
    // Admission is idempotent.
    let again: Value = serde_json::from_str(&ok(&mote(&a, &["--json", "admit"]))).unwrap();
    assert_eq!(again["admitted"], serde_json::json!([]));
    assert!(again["record"].is_null());

    for dir in [&a, &b] {
        ok(&mote(dir, &["fsck"]));
        assert_eq!(tracked_changes(dir), "", "admission changed a tracked file");
    }
    let a_board = board(&a);
    assert_eq!(a_board, board(&b));
    let shown = a_board.to_string();
    for id in [&a_work, &b_work] {
        assert!(shown.contains(id.as_str()), "{id} missing from {shown}");
    }
    let show = |dir: &Path| {
        ok(&mote(
            dir,
            &["--actor", "observer", "--json", "show", &shared],
        ))
    };
    assert_eq!(show(&a), show(&b));
    assert!(show(&a).contains("from b"));
    // Both replicas admitted the same op set, each in its own order.
    let names = |dir: &Path| {
        let mut names = Store::open(&dir.join(".mote"))
            .unwrap()
            .list_op_filenames()
            .unwrap();
        names.sort();
        names
    };
    assert_eq!(names(&a), names(&b));
    // Replicas stay usable: a new fenced write lands after the admitted ops.
    ok(&mote(
        &a,
        &[
            "--actor",
            "alice",
            "note",
            "--kind",
            "note",
            &shared,
            "after admit",
        ],
    ));
}

fn foreign_op(issue: &str) -> (String, Vec<u8>) {
    let other = TempDir::new().unwrap();
    let store = Store::init(other.path()).unwrap();
    let name = publish::publish_op(
        &store,
        &make_create(
            "remote".into(),
            issue.into(),
            ScalarSet {
                title: Some("foreign".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    let file = format!("{}.json", name.as_str());
    let bytes = std::fs::read(store.ops_dir().join(&file)).unwrap();
    (file, bytes)
}

fn fenced_store() -> (TempDir, Store) {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            ids::new_bead_id(),
            ScalarSet {
                title: Some("local".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    Writer::acquire(&store).unwrap().enable().unwrap();
    (root, store)
}

fn record_count(store: &Store) -> usize {
    std::fs::read_dir(authority::directory(store))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().into_string().unwrap();
            name.len() == 25 && name.as_bytes()[0].is_ascii_digit()
        })
        .count()
}

#[test]
fn unadmitted_op_error_is_actionable_and_admit_appends_after_the_prefix() {
    let (root, store) = fenced_store();
    let before = store.list_op_filenames().unwrap();
    let issue = ids::new_bead_id();
    let (file, bytes) = foreign_op(&issue);
    std::fs::write(store.ops_dir().join(&file), bytes).unwrap();

    let error = reducer::replay_store(&store).unwrap_err().to_string();
    assert!(error.contains("unadmitted operation"), "{error}");
    assert!(error.contains("run `mote admit`"), "{error}");
    let cli_error = fails(&mote(root.path(), &["--actor", "observer", "ls"]));
    assert!(cli_error.contains("mote admit"), "{cli_error}");

    let text = ok(&mote(root.path(), &["admit"]));
    assert!(text.contains("admitted 1 op(s)"), "{text}");
    let after = store.list_op_filenames().unwrap();
    assert_eq!(&after[..before.len()], before.as_slice());
    assert_eq!(after.last(), Some(&file));
    assert!(
        reducer::replay_store(&store)
            .unwrap()
            .beads
            .contains_key(&issue)
    );
    ok(&mote(root.path(), &["fsck"]));
}

#[test]
fn admit_reports_ops_that_sort_before_the_admitted_tail() {
    let (root, store) = fenced_store();
    let old = TempDir::new().unwrap();
    let other = Store::init(old.path()).unwrap();
    let earlier: Timestamp = "2020-01-01T00:00:00Z".parse().unwrap();
    let name = publish::publish_op(
        &other,
        &make_create(
            "remote".into(),
            ids::new_bead_id(),
            ScalarSet {
                title: Some("early".into()),
                ..Default::default()
            },
            earlier,
        ),
    )
    .unwrap();
    let file = format!("{}.json", name.as_str());
    std::fs::copy(other.ops_dir().join(&file), store.ops_dir().join(&file)).unwrap();
    let report: Value =
        serde_json::from_str(&ok(&mote(root.path(), &["--json", "admit"]))).unwrap();
    assert_eq!(report["out_of_order"], serde_json::json!([file.clone()]));
    // Admission order, not op-id order, governs replay.
    assert_eq!(store.list_op_filenames().unwrap().last(), Some(&file));
}

#[test]
fn admit_fails_closed_on_changed_admitted_bytes_and_on_invalid_foreign_ops() {
    let (root, store) = fenced_store();
    let admitted = store.list_op_filenames().unwrap()[0].clone();
    let records = record_count(&store);

    // Same op id, different bytes: Git replaced an admitted op.
    let path = store.ops_dir().join(&admitted);
    let original = std::fs::read(&path).unwrap();
    let changed = String::from_utf8(original.clone())
        .unwrap()
        .replace("local", "LOCAL");
    std::fs::write(&path, changed).unwrap();
    let error = fails(&mote(root.path(), &["admit"]));
    assert!(error.contains("authority op changed"), "{error}");
    assert!(error.contains(&admitted), "{error}");
    assert_eq!(record_count(&store), records);
    std::fs::write(&path, original).unwrap();

    // A batch with one invalid op admits nothing, valid ops included.
    let (valid, valid_bytes) = foreign_op(&ids::new_bead_id());
    std::fs::write(store.ops_dir().join(&valid), valid_bytes).unwrap();
    let (bad, bad_bytes) = foreign_op(&ids::new_bead_id());
    let tampered = String::from_utf8(bad_bytes)
        .unwrap()
        .replace("foreign", "tampered");
    std::fs::write(store.ops_dir().join(&bad), tampered).unwrap();
    let error = fails(&mote(root.path(), &["admit"]));
    assert!(error.contains("refusing to admit"), "{error}");
    assert!(error.contains(&bad), "{error}");
    assert!(error.contains("hash mismatch"), "{error}");
    assert_eq!(record_count(&store), records);
    assert!(
        reducer::replay_store(&store)
            .unwrap_err()
            .to_string()
            .contains("unadmitted")
    );

    std::fs::remove_file(store.ops_dir().join(&bad)).unwrap();
    ok(&mote(root.path(), &["admit"]));
    assert_eq!(record_count(&store), records + 1);
}

#[test]
fn missing_admitted_op_names_the_restore_remedy() {
    let (_root, store) = fenced_store();
    let admitted = store.list_op_filenames().unwrap()[0].clone();
    std::fs::remove_file(store.ops_dir().join(&admitted)).unwrap();
    let error = reducer::replay_store(&store).unwrap_err().to_string();
    assert!(error.contains("missing from ops/"), "{error}");
    assert!(error.contains(&admitted), "{error}");
}

#[test]
#[cfg(debug_assertions)]
fn admit_recovers_a_pending_publication_before_foreign_ops() {
    let (root, store) = fenced_store();
    let issue = reducer::replay_store(&store)
        .unwrap()
        .beads
        .keys()
        .next()
        .unwrap()
        .clone();
    let interrupted = Command::new(env!("CARGO_BIN_EXE_mote"))
        .args([
            "--actor", "alice", "note", "--kind", "note", &issue, "pending",
        ])
        .current_dir(root.path())
        .env_remove("MOTE_STORE")
        .env("MOTE_TEST_AUTHORITY_FAIL", "publication-prepared")
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    assert!(
        authority::directory(&store)
            .join("publication.json")
            .exists()
    );
    let (file, bytes) = foreign_op(&ids::new_bead_id());
    std::fs::write(store.ops_dir().join(&file), bytes).unwrap();

    let report: Value =
        serde_json::from_str(&ok(&mote(root.path(), &["--json", "admit"]))).unwrap();
    assert_eq!(report["admitted"], serde_json::json!([file.clone()]));
    assert!(
        !authority::directory(&store)
            .join("publication.json")
            .exists()
    );
    let names = store.list_op_filenames().unwrap();
    assert_eq!(names.last(), Some(&file));
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&issue].notes.len(), 1);
}

/// Reproduce what earlier binaries wrote: the binding in FORMAT.json only.
fn legacy_binding(store: &Store) -> Value {
    let binding: Value =
        serde_json::from_slice(&std::fs::read(authority::binding_path(store)).unwrap()).unwrap();
    std::fs::remove_file(authority::binding_path(store)).unwrap();
    let mut format: Value =
        serde_json::from_slice(&std::fs::read(store.format_path()).unwrap()).unwrap();
    format["authority"] = binding.clone();
    std::fs::write(store.format_path(), serde_json::to_vec(&format).unwrap()).unwrap();
    binding
}

#[test]
fn legacy_format_binding_migrates_to_local_state_with_the_same_genesis() {
    let (root, store) = fenced_store();
    let pristine = std::fs::read(store.format_path()).unwrap();
    assert!(
        !String::from_utf8_lossy(&pristine).contains("authority"),
        "activation must not write FORMAT.json"
    );
    let genesis = authority::status(&store).unwrap().genesis_digest;
    let legacy = legacy_binding(&store);
    let legacy_bytes = std::fs::read(store.format_path()).unwrap();

    // Unmigrated stores still read and report through FORMAT.json.
    let status = authority::status(&store).unwrap();
    assert_eq!(status.binding, Some(BindingLocation::Format));
    assert_eq!(status.genesis_digest, genesis);
    reducer::replay_store(&store).unwrap();

    // Any writer mirrors the binding locally without touching FORMAT.json.
    drop(Writer::acquire(&store).unwrap());
    assert_eq!(std::fs::read(store.format_path()).unwrap(), legacy_bytes);
    let status = authority::status(&store).unwrap();
    assert_eq!(status.binding, Some(BindingLocation::Local));
    assert!(!status.foreign_format_binding);

    // An earlier binary may raise only the FORMAT.json version; it still counts.
    let mut format: Value = serde_json::from_slice(&legacy_bytes).unwrap();
    format["authority"]["version"] = 2.into();
    std::fs::write(store.format_path(), serde_json::to_vec(&format).unwrap()).unwrap();
    assert_eq!(authority::status(&store).unwrap().authority_version, 2);

    let migrated: Value =
        serde_json::from_str(&ok(&mote(root.path(), &["authority", "migrate"]))).unwrap();
    assert_eq!(migrated["format_binding_removed"], true);
    assert_eq!(migrated["binding"], "local");
    assert_eq!(migrated["authority_version"], 2);
    assert_eq!(migrated["genesis_digest"], legacy["genesis_hash"]);
    assert_eq!(std::fs::read(store.format_path()).unwrap(), pristine);
    let repeated: Value =
        serde_json::from_str(&ok(&mote(root.path(), &["authority", "migrate"]))).unwrap();
    assert_eq!(repeated["format_binding_removed"], false);
    reducer::replay_store(&store).unwrap();
}

#[test]
fn foreign_format_binding_from_another_clone_is_discarded_explicitly() {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    let pristine = std::fs::read(store.format_path()).unwrap();
    // A fresh clone received FORMAT.json committed by a fenced replica.
    let mut format: Value = serde_json::from_slice(&pristine).unwrap();
    format["authority"] = serde_json::json!({"version": 1, "genesis_hash": "0".repeat(64)});
    std::fs::write(store.format_path(), serde_json::to_vec(&format).unwrap()).unwrap();

    let error = fails(&mote(root.path(), &["--actor", "observer", "ls"]));
    assert!(error.contains("--discard-foreign-format"), "{error}");
    fails(&mote(root.path(), &["authority", "migrate"]));

    let discarded: Value = serde_json::from_str(&ok(&mote(
        root.path(),
        &["authority", "migrate", "--discard-foreign-format"],
    )))
    .unwrap();
    assert_eq!(discarded["foreign_format_binding_discarded"], true);
    assert_eq!(discarded["enabled"], false);
    assert_eq!(std::fs::read(store.format_path()).unwrap(), pristine);
    ok(&mote(root.path(), &["--actor", "observer", "ls"]));

    // A locally bound replica keeps its own binding and drops only the foreign one.
    Writer::acquire(&store).unwrap().enable().unwrap();
    std::fs::write(store.format_path(), serde_json::to_vec(&format).unwrap()).unwrap();
    let status = authority::status(&store).unwrap();
    assert!(status.foreign_format_binding);
    reducer::replay_store(&store).unwrap();
    let discarded: Value = serde_json::from_str(&ok(&mote(
        root.path(),
        &["authority", "migrate", "--discard-foreign-format"],
    )))
    .unwrap();
    assert_eq!(discarded["foreign_format_binding_discarded"], true);
    assert_eq!(discarded["enabled"], true);
    assert_eq!(std::fs::read(store.format_path()).unwrap(), pristine);

    // A replica that lost authority/ keeps its own legacy binding and stays
    // fenced: the flag removes only another replica's binding.
    let own = legacy_binding(&store);
    std::fs::write(
        authority::binding_path(&store),
        serde_json::to_vec(&own).unwrap(),
    )
    .unwrap();
    std::fs::remove_dir_all(authority::directory(&store)).unwrap();
    fails(&mote(
        root.path(),
        &["authority", "migrate", "--discard-foreign-format"],
    ));
    assert!(
        String::from_utf8_lossy(&std::fs::read(store.format_path()).unwrap())
            .contains(own["genesis_hash"].as_str().unwrap())
    );
    assert!(reducer::replay_store(&store).is_err());
}

#[test]
fn doctor_warns_when_git_tracks_per_replica_authority_state() {
    let (_td, a, _b, id) = two_replicas();
    ok(&mote(
        &a,
        &["--actor", "alice", "begin", &id, "--paths", "src/a"],
    ));
    let doctor = |dir: &Path| -> Vec<String> {
        let report: Value =
            serde_json::from_str(&ok(&mote(dir, &["--actor", "alice", "--json", "doctor"])))
                .unwrap();
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap().to_string())
            .collect()
    };
    assert!(
        !doctor(&a).iter().any(|w| w.contains("authority")),
        "{:?}",
        doctor(&a)
    );

    git(&a, &["add", "-f", ".mote/authority"]);
    let warnings = doctor(&a);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("Git tracks") && w.contains("authority")),
        "{warnings:?}"
    );

    git(&a, &["rm", "-q", "-r", "--cached", ".mote/authority"]);
    std::fs::write(a.join(".gitignore"), ".mote/local/\n.mote/tmp/\n").unwrap();
    let warnings = doctor(&a);
    assert!(
        warnings.iter().any(|w| w.contains("is not ignored")),
        "{warnings:?}"
    );
}

#[test]
fn conflicting_claims_from_two_replicas_resolve_in_each_replicas_admission_order() {
    // Pins known behaviour: replicas agree on the op set, not on the order, so
    // order-sensitive ops such as two claims on one issue can diverge.
    let (_td, a, b, shared) = two_replicas();
    ok(&mote(&a, &["--actor", "alice", "claim", &shared]));
    ok(&mote(&b, &["--actor", "bob", "claim", &shared]));
    publish_ops_and_sync(&a);
    publish_ops_and_sync(&b);
    git(
        &a,
        &["pull", "-q", "--no-rebase", "--no-edit", "origin", "main"],
    );
    ok(&mote(&a, &["admit"]));
    ok(&mote(&b, &["admit"]));
    let holder = |dir: &Path| {
        board(dir)["active_claims"]
            .as_array()
            .unwrap()
            .iter()
            .find(|claim| claim["id"] == shared.as_str())
            .map(|claim| claim["claimed_by"].clone())
    };
    assert_eq!(holder(&a), Some("alice".into()));
    assert_eq!(holder(&b), Some("bob".into()));
}

#[test]
fn unadmitted_ops_wait_while_a_landing_needs_recovery() {
    let (root, store) = fenced_store();
    let (file, bytes) = foreign_op(&ids::new_bead_id());
    std::fs::write(store.ops_dir().join(&file), bytes).unwrap();
    let active = authority::directory(&store).join("landing-active.json");
    std::fs::write(&active, b"{}").unwrap();

    // The landing's admitted prefix stays readable so it can be recovered...
    ok(&mote(root.path(), &["--actor", "observer", "ls"]));
    assert!(!store.list_op_filenames().unwrap().contains(&file));
    // ...and nothing may extend that prefix until it is.
    let error = fails(&mote(root.path(), &["admit"]));
    assert!(error.contains("candidate land"), "{error}");
    assert!(error.contains("mote admit"), "{error}");

    std::fs::remove_file(&active).unwrap();
    assert!(
        reducer::replay_store(&store)
            .unwrap_err()
            .to_string()
            .contains("unadmitted")
    );
    ok(&mote(root.path(), &["admit"]));
    assert_eq!(store.list_op_filenames().unwrap().last(), Some(&file));
}

#[test]
fn admit_accepts_an_op_kind_this_binary_does_not_know() {
    // A clone on a newer binary may publish a new kind. Replay records it as
    // rejected history; refusing it here would brick every older replica.
    let (root, store) = fenced_store();
    let other = TempDir::new().unwrap();
    let newer = Store::init(other.path()).unwrap();
    let name = publish::publish_value(
        &newer,
        serde_json::json!({"kind": "future_kind", "actor": "remote"}),
        Timestamp::now(),
    )
    .unwrap();
    let file = format!("{}.json", name.as_str());
    std::fs::copy(newer.ops_dir().join(&file), store.ops_dir().join(&file)).unwrap();

    let report: Value =
        serde_json::from_str(&ok(&mote(root.path(), &["--json", "admit"]))).unwrap();
    assert_eq!(report["admitted"], serde_json::json!([file.clone()]));
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
}
