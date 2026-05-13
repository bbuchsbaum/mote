//! M2 acceptance tests:
//!   A1 — 20 concurrent `mote new` produce 20 distinct beads.
//!   A2 — same-field patch race: exactly one accepted.
//!   A9 — `mote history --include-rejected` exposes the rejected patch.
//! Plus an end-to-end CLI smoke covering new / set / show / ls / close.

use std::collections::{BTreeMap, HashSet};
use std::process::Command;
use std::thread;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::op::{ScalarSet, Status, make_create, make_patch};
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
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Store::open(&td.path().join(".mote")).unwrap()
}

#[test]
fn a1_concurrent_creates_via_processes() {
    let td = TempDir::new().unwrap();
    init_store(&td);

    let mut handles = Vec::new();
    for i in 0..20u32 {
        let bin = mote_bin().to_string();
        let dir = td.path().to_path_buf();
        handles.push(thread::spawn(move || -> String {
            let out = Command::new(bin)
                .args(["new", &format!("title-{i}"), "--actor", &format!("a{i}")])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "mote new failed (i={i}): stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }));
    }
    let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let unique: HashSet<&String> = ids.iter().collect();
    assert_eq!(
        unique.len(),
        20,
        "expected 20 unique bead ids, got {}",
        unique.len()
    );

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let op_files = store.list_op_filenames().unwrap();
    assert_eq!(
        op_files.len(),
        20,
        "expected 20 op files, got {}",
        op_files.len()
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads.len(), 20);
    for id in &ids {
        assert!(state.beads.contains_key(id));
    }
}

#[test]
fn a2_same_field_race_via_library() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    let entity = "bd-test-A2".to_string();
    let create = make_create(
        "alice".into(),
        entity.clone(),
        ScalarSet {
            title: Some("a".into()),
            ..Default::default()
        },
        Timestamp::now(),
    );
    publish::publish_op(&store, &create).unwrap();

    // Both racers replay BEFORE either publishes.
    let state1 = reducer::replay_store(&store).unwrap();
    let state2 = reducer::replay_store(&store).unwrap();
    let clk1 = state1.beads[&entity].clock["status"].clone();
    let clk2 = state2.beads[&entity].clock["status"].clone();
    assert_eq!(clk1, clk2);

    let mut expect = BTreeMap::new();
    expect.insert("status".to_string(), clk1);

    let p1 = make_patch(
        "alice".into(),
        entity.clone(),
        expect.clone(),
        ScalarSet {
            status: Some(Status::Doing),
            ..Default::default()
        },
        Timestamp::now(),
    );
    let p2 = make_patch(
        "bob".into(),
        entity.clone(),
        expect,
        ScalarSet {
            status: Some(Status::Blocked),
            ..Default::default()
        },
        Timestamp::now(),
    );
    let n1 = publish::publish_op(&store, &p1).unwrap();
    let n2 = publish::publish_op(&store, &p2).unwrap();

    let final_state = reducer::replay_store(&store).unwrap();
    let a1 = final_state.was_accepted(n1.as_str());
    let a2 = final_state.was_accepted(n2.as_str());
    assert!(
        a1 ^ a2,
        "exactly one of {{p1, p2}} must be accepted (a1={a1}, a2={a2})"
    );

    let losing_id = if a1 { n2.as_str() } else { n1.as_str() };
    let reason = final_state
        .rejection_reason(losing_id)
        .expect("losing op must carry a reason");
    assert!(
        reason.contains("stale"),
        "expected 'stale' in reason, got {reason:?}"
    );
}

#[test]
fn a9_history_include_rejected_via_cli() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    let entity = "bd-test-A9".to_string();
    let create = make_create(
        "alice".into(),
        entity.clone(),
        ScalarSet {
            title: Some("h".into()),
            ..Default::default()
        },
        Timestamp::now(),
    );
    publish::publish_op(&store, &create).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let clk = state.beads[&entity].clock["status"].clone();

    let mut expect = BTreeMap::new();
    expect.insert("status".to_string(), clk);
    let p1 = make_patch(
        "alice".into(),
        entity.clone(),
        expect.clone(),
        ScalarSet {
            status: Some(Status::Doing),
            ..Default::default()
        },
        Timestamp::now(),
    );
    let p2 = make_patch(
        "bob".into(),
        entity.clone(),
        expect,
        ScalarSet {
            status: Some(Status::Blocked),
            ..Default::default()
        },
        Timestamp::now(),
    );
    publish::publish_op(&store, &p1).unwrap();
    publish::publish_op(&store, &p2).unwrap();

    // Default history → only accepts.
    let out_default = Command::new(mote_bin())
        .args(["history", &entity])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out_default.status.success());
    let s_default = String::from_utf8(out_default.stdout).unwrap();
    assert!(
        !s_default.contains("REJECT"),
        "default history must not include rejected:\n{s_default}"
    );

    // --include-rejected → has a REJECT line with 'stale'.
    let out_full = Command::new(mote_bin())
        .args(["history", &entity, "--include-rejected"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out_full.status.success());
    let s_full = String::from_utf8(out_full.stdout).unwrap();
    assert!(s_full.contains("REJECT"), "expected REJECT line:\n{s_full}");
    assert!(
        s_full.contains("stale"),
        "expected 'stale' in reason:\n{s_full}"
    );
}

#[test]
fn cli_smoke_new_set_show_ls_close() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let new_out = Command::new(bin)
        .args([
            "new", "Fix auth", "-p", "1", "--actor", "tester", "--tag", "backend",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        new_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    let id = String::from_utf8(new_out.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(id.starts_with("bd-"));

    let set_out = Command::new(bin)
        .args(["set", &id, "status=doing", "--actor", "tester"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        set_out.status.success(),
        "set stderr: {}",
        String::from_utf8_lossy(&set_out.stderr)
    );

    let show_out = Command::new(bin)
        .args(["show", &id])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(show_out.status.success());
    let show_s = String::from_utf8(show_out.stdout).unwrap();
    assert!(show_s.contains("status:"));
    assert!(show_s.contains("doing"));
    assert!(show_s.contains("backend"));

    let ls_out = Command::new(bin)
        .args(["ls", "--status", "doing"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ls_out.status.success());
    let ls_s = String::from_utf8(ls_out.stdout).unwrap();
    assert!(ls_s.contains(&id));

    let close_out = Command::new(bin)
        .args(["close", &id, "--actor", "tester"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        close_out.status.success(),
        "close stderr: {}",
        String::from_utf8_lossy(&close_out.stderr)
    );

    // After close, default `ls` hides closed beads.
    let ls2 = Command::new(bin)
        .args(["ls"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ls2.status.success());
    let ls2_s = String::from_utf8(ls2.stdout).unwrap();
    assert!(
        !ls2_s.contains(&id),
        "closed bead should be hidden by default:\n{ls2_s}"
    );

    // `--all` reveals it.
    let ls3 = Command::new(bin)
        .args(["ls", "--all"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ls3.status.success());
    let ls3_s = String::from_utf8(ls3.stdout).unwrap();
    assert!(ls3_s.contains(&id));
    assert!(ls3_s.contains("closed"));
}

#[test]
fn dep_add_and_remove_round_trip_via_cli() {
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

    let add = Command::new(bin)
        .args(["dep", "add", &child, &parent, "--actor", "tester"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        add.status.success(),
        "dep add stderr: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    let show = Command::new(bin)
        .args(["show", &child])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s = String::from_utf8(show.stdout).unwrap();
    assert!(
        s.contains(&parent),
        "expected parent in child's show output:\n{s}"
    );

    let rm = Command::new(bin)
        .args(["dep", "rm", &child, &parent, "--actor", "tester"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(rm.status.success());

    let show2 = Command::new(bin)
        .args(["show", &child])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s2 = String::from_utf8(show2.stdout).unwrap();
    assert!(
        !s2.contains(&parent),
        "expected parent gone from child's show:\n{s2}"
    );
}

#[test]
fn new_with_custom_id_preserves_external_id() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let out = Command::new(bin)
        .args([
            "new",
            "Fix auth bug",
            "--actor",
            "alice",
            "--id",
            "psycloud-eqgu",
            "-p",
            "1",
            "--tag",
            "bug",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(printed, "psycloud-eqgu");

    let show = Command::new(bin)
        .args(["show", "psycloud-eqgu"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(show.status.success());
    let s = String::from_utf8(show.stdout).unwrap();
    assert!(s.contains("psycloud-eqgu"), "show output:\n{s}");
    assert!(s.contains("Fix auth bug"));
    assert!(s.contains("bug"));
}

#[test]
fn new_with_duplicate_custom_id_is_rejected_by_reducer() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    let first = Command::new(bin)
        .args(["new", "first", "--actor", "alice", "--id", "dup-1"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(first.status.success());

    let second = Command::new(bin)
        .args(["new", "second", "--actor", "bob", "--id", "dup-1"])
        .current_dir(td.path())
        .output()
        .unwrap();
    // Reducer rejects → exit code 2.
    assert_eq!(second.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&second.stderr).to_string();
    assert!(
        stderr.contains("already exists"),
        "expected reducer rejection on stderr, got:\n{stderr}"
    );
}

#[test]
fn new_with_invalid_custom_id_fails_validation() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let bin = mote_bin();

    for bad in ["bd-anything", "Has-Caps", "has space", ""] {
        let out = Command::new(bin)
            .args(["new", "title", "--actor", "alice", "--id", bad])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "expected failure for --id {bad:?}; stdout={}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}
