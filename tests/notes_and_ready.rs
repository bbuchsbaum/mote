//! M3 acceptance: A8 (5 concurrent notes never conflict) + ready computation.

use std::collections::BTreeMap;
use std::process::Command;
use std::thread;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::op::{ScalarSet, Status, make_close, make_create, make_dep, make_rel};
use mote::{publish, reducer, repo::Store};

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_store(td: &TempDir) -> Store {
    let out = Command::new(mote_bin())
        .arg("init")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed");
    Store::open(&td.path().join(".mote")).unwrap()
}

#[test]
fn design_is_a_valid_note_kind() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let created = Command::new(mote_bin())
        .args(["new", "design note", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let id = String::from_utf8(created.stdout)
        .unwrap()
        .trim()
        .to_string();
    let note = Command::new(mote_bin())
        .args([
            "note",
            &id,
            "--kind",
            "design",
            "chosen shape",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        note.status.success(),
        "{}",
        String::from_utf8_lossy(&note.stderr)
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&id].notes[0].note_kind, "design");
}

#[test]
fn a8_concurrent_notes_all_accept() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    // Create one bead first.
    let new_out = Command::new(bin)
        .args(["new", "shared", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(new_out.status.success(), "new failed");
    let id = String::from_utf8(new_out.stdout)
        .unwrap()
        .trim()
        .to_string();

    // Spawn 5 concurrent `mote note` invocations on the same bead.
    let mut handles = Vec::new();
    for i in 0..5u32 {
        let bin = bin.to_string();
        let dir = td.path().to_path_buf();
        let id = id.clone();
        handles.push(thread::spawn(move || {
            let out = Command::new(bin)
                .args([
                    "note",
                    &id,
                    "--kind",
                    "progress",
                    &format!("note from agent-{i}"),
                    "--actor",
                    &format!("agent-{i}"),
                ])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "note failed (i={i}): stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Replay: 5 notes attached to the bead, all accepted.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    assert_eq!(
        bead.notes.len(),
        5,
        "expected 5 notes, got {}",
        bead.notes.len()
    );

    let history = state
        .history
        .get(&id)
        .expect("history must contain entries");
    let accepted_notes = history
        .iter()
        .filter(|e| e.kind == "note" && e.accepted)
        .count();
    assert_eq!(accepted_notes, 5);
    let rejected_notes = history
        .iter()
        .filter(|e| e.kind == "note" && !e.accepted)
        .count();
    assert_eq!(rejected_notes, 0);
}

#[test]
fn note_invalid_kind_is_rejected_at_cli() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let new_out = Command::new(bin)
        .args(["new", "x", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let id = String::from_utf8(new_out.stdout)
        .unwrap()
        .trim()
        .to_string();

    let bad = Command::new(bin)
        .args([
            "note",
            &id,
            "--kind",
            "rationale",
            "txt",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        !bad.status.success(),
        "expected failure for invalid note_kind"
    );
    let stderr = String::from_utf8(bad.stderr).unwrap();
    assert!(
        stderr.contains("invalid note_kind"),
        "expected 'invalid note_kind' in stderr, got: {stderr}"
    );
}

#[test]
fn ready_computation_dep_graph() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    // Build by hand: A (no deps), Z (closed), B (deps on A), C (deps on Z, closed)
    // Expected ready: A, C  (B blocked by open A; closed-deps-only beads are ready)
    let ts0 = Timestamp::now();
    let mk_create = |id: &str, status: Status, ts: Timestamp| {
        let mut s = ScalarSet {
            title: Some(id.into()),
            ..Default::default()
        };
        s.status = Some(status);
        publish::publish_op(&store, &make_create("tester".into(), id.into(), s, ts)).unwrap()
    };

    let _na = mk_create("bd-A", Status::Open, ts0);
    let _nz = mk_create("bd-Z", Status::Open, ts0);

    // Close Z via close op.
    let pre_state = reducer::replay_store(&store).unwrap();
    let z_status_clk = pre_state.beads["bd-Z"].clock["status"].clone();
    let mut expect = BTreeMap::new();
    expect.insert("status".to_string(), z_status_clk);
    publish::publish_op(
        &store,
        &make_close("tester".into(), "bd-Z".into(), expect, Timestamp::now()),
    )
    .unwrap();

    let _nb = mk_create("bd-B", Status::Open, Timestamp::now());
    let _nc = mk_create("bd-C", Status::Open, Timestamp::now());

    // dep_add: B blocked by A; C blocked by Z (closed).
    publish::publish_op(
        &store,
        &make_dep(
            true,
            "tester".into(),
            "bd-B".into(),
            "bd-A".into(),
            "blocks".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_dep(
            true,
            "tester".into(),
            "bd-C".into(),
            "bd-Z".into(),
            "blocks".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    let ready_ids: Vec<&str> = state.ready_beads().map(|b| b.id.as_str()).collect();

    assert!(
        ready_ids.contains(&"bd-A"),
        "A should be ready: {ready_ids:?}"
    );
    assert!(
        ready_ids.contains(&"bd-C"),
        "C should be ready (parent closed): {ready_ids:?}"
    );
    assert!(
        !ready_ids.contains(&"bd-B"),
        "B should NOT be ready (parent open): {ready_ids:?}"
    );
    assert!(
        !ready_ids.contains(&"bd-Z"),
        "Z should NOT be ready (status closed): {ready_ids:?}"
    );
}

#[test]
fn ready_ignores_non_blocking_relations() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    let mk_create = |id: &str| {
        let mut s = ScalarSet {
            title: Some(id.into()),
            ..Default::default()
        };
        s.status = Some(Status::Open);
        publish::publish_op(
            &store,
            &make_create("tester".into(), id.into(), s, Timestamp::now()),
        )
        .unwrap()
    };

    mk_create("bd-epic");
    mk_create("bd-leaf");
    publish::publish_op(
        &store,
        &make_rel(
            true,
            "tester".into(),
            "bd-leaf".into(),
            "bd-epic".into(),
            "parent".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    let ready_ids: Vec<&str> = state.ready_beads().map(|b| b.id.as_str()).collect();
    assert!(
        ready_ids.contains(&"bd-leaf"),
        "non-blocking relation parent should not hide leaf from ready: {ready_ids:?}"
    );
    assert_eq!(state.relation_children_of("bd-epic")[0].0.id, "bd-leaf");
}

#[test]
fn ready_cli_command_returns_expected_set() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let mk = |title: &str| -> String {
        let out = Command::new(bin)
            .args(["new", title, "--actor", "tester"])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let parent = mk("parent");
    let child = mk("child");
    let _ = Command::new(bin)
        .args(["dep", "add", &child, &parent, "--actor", "tester"])
        .current_dir(td.path())
        .output()
        .unwrap();

    // Before parent is closed, only `parent` is ready.
    let ready_out = Command::new(bin)
        .arg("ready")
        .current_dir(td.path())
        .output()
        .unwrap();
    let r = String::from_utf8(ready_out.stdout).unwrap();
    assert!(r.contains(&parent), "parent should be ready: {r}");
    assert!(!r.contains(&child), "child should NOT be ready: {r}");

    // Close parent → child becomes ready.
    let close_out = Command::new(bin)
        .args(["close", &parent, "--actor", "tester"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(close_out.status.success());

    let ready_out2 = Command::new(bin)
        .arg("ready")
        .current_dir(td.path())
        .output()
        .unwrap();
    let r2 = String::from_utf8(ready_out2.stdout).unwrap();
    assert!(
        !r2.contains(&parent),
        "parent should NOT be ready (closed): {r2}"
    );
    assert!(
        r2.contains(&child),
        "child should be ready now (parent closed): {r2}"
    );

    // `ls --ready` is the documented alias and must produce the same set.
    let ls_ready = Command::new(bin)
        .args(["ls", "--ready"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let lr = String::from_utf8(ls_ready.stdout).unwrap();
    assert!(
        lr.contains(&child),
        "ls --ready should also list child: {lr}"
    );
    assert!(!lr.contains(&parent));
}
