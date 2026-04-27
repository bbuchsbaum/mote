//! Stress and performance smoke tests.
//!
//! Runs are bounded so they fit in a normal `cargo test` invocation. The
//! larger 10k-op smoke is `#[ignore]`d so contributors who want to run it
//! pass `cargo test -- --ignored`.

use std::collections::BTreeMap;
use std::process::Command;
use std::thread;
use std::time::Instant;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::op::{ScalarSet, Status, make_create, make_dep, make_patch, make_tag};
use mote::{publish, reducer, repo::Store};

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_store_dir(td: &TempDir) {
    let out = Command::new(mote_bin())
        .arg("init")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed");
}

#[test]
fn stress_50_concurrent_creates() {
    let td = TempDir::new().unwrap();
    init_store_dir(&td);

    let mut handles = Vec::with_capacity(50);
    for i in 0..50u32 {
        let bin = mote_bin().to_string();
        let dir = td.path().to_path_buf();
        handles.push(thread::spawn(move || -> String {
            let out = Command::new(bin)
                .args(["new", &format!("t{i}"), "--actor", &format!("a{i}")])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "create failed (i={i}): {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        }));
    }
    let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 50);

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let names = store.list_op_filenames().unwrap();
    assert_eq!(names.len(), 50);
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads.len(), 50);
}

#[test]
fn stress_repeated_begin_race() {
    // Re-run the A4 race 10× back-to-back. In every round, exactly one of two
    // overlapping `mote begin` calls must succeed; the other must exit 2 with
    // a reserve_open conflict.
    for round in 0..10u32 {
        let td = TempDir::new().unwrap();
        init_store_dir(&td);

        let new_out = Command::new(mote_bin())
            .args(["new", "auth", "--actor", "alice"])
            .current_dir(td.path())
            .output()
            .unwrap();
        assert!(new_out.status.success());
        let id = String::from_utf8(new_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        let dir = td.path().to_path_buf();
        let id_a = id.clone();
        let id_b = id.clone();

        let h1 = thread::spawn(move || {
            Command::new(mote_bin())
                .args(["begin", &id_a, "--paths", "src/auth/", "--actor", "alpha"])
                .current_dir(&dir)
                .output()
                .unwrap()
        });
        let dir2 = td.path().to_path_buf();
        let h2 = thread::spawn(move || {
            Command::new(mote_bin())
                .args([
                    "begin",
                    &id_b,
                    "--paths",
                    "src/auth/token.rs",
                    "--actor",
                    "beta",
                ])
                .current_dir(&dir2)
                .output()
                .unwrap()
        });
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        let s1 = r1.status.success();
        let s2 = r2.status.success();
        assert!(
            s1 ^ s2,
            "round {round}: exactly one begin must succeed (s1={s1} s2={s2})"
        );
        let loser = if s1 { r2 } else { r1 };
        assert_eq!(
            loser.status.code(),
            Some(2),
            "round {round}: loser must exit 2"
        );
    }
}

/// Replay performance smoke on 1000 ops. PRD targets "instant" at 1k.
/// We assert <2s on dev hardware; in practice it lands well under 200ms.
#[test]
fn perf_smoke_1k_ops_replay() {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();

    // Build 50 beads, then sprinkle ~950 mutations across them.
    let mut bead_ids = Vec::new();
    for i in 0..50 {
        let id = format!("bd-{i:04}");
        publish::publish_op(
            &store,
            &make_create(
                "alice".into(),
                id.clone(),
                ScalarSet {
                    title: Some(format!("t{i}")),
                    ..Default::default()
                },
                Timestamp::now(),
            ),
        )
        .unwrap();
        bead_ids.push(id);
    }

    let mut total_ops = 50;
    while total_ops < 1000 {
        let i = total_ops % bead_ids.len();
        let id = bead_ids[i].clone();
        let op = match total_ops % 4 {
            0 => make_patch(
                "alice".into(),
                id,
                BTreeMap::new(),
                ScalarSet {
                    status: Some(Status::Doing),
                    ..Default::default()
                },
                Timestamp::now(),
            ),
            1 => make_tag(
                true,
                "alice".into(),
                id,
                format!("tag-{}", total_ops % 7),
                Timestamp::now(),
            ),
            2 => {
                let parent = bead_ids[(i + 1) % bead_ids.len()].clone();
                make_dep(
                    true,
                    "alice".into(),
                    id,
                    parent,
                    "blocks".into(),
                    Timestamp::now(),
                )
            }
            _ => mote::op::make_note(
                "alice".into(),
                id,
                "progress".into(),
                format!("step {total_ops}"),
                Timestamp::now(),
            ),
        };
        let _ = publish::publish_op(&store, &op);
        total_ops += 1;
    }

    let names = store.list_op_filenames().unwrap();
    assert!(names.len() >= 1000, "expected 1k ops, got {}", names.len());

    let start = Instant::now();
    let state = reducer::replay_store(&store).unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 2000,
        "1k op replay took {}ms (target: <2000ms)",
        elapsed.as_millis()
    );
    assert_eq!(state.beads.len(), 50);
}

/// Larger smoke at 10k ops. Ignored by default; run with:
///   cargo test --test stress perf_smoke_10k_ops_replay -- --ignored --nocapture
#[test]
#[ignore]
fn perf_smoke_10k_ops_replay() {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();

    let mut bead_ids = Vec::new();
    for i in 0..200 {
        let id = format!("bd-{i:04}");
        publish::publish_op(
            &store,
            &make_create(
                "alice".into(),
                id.clone(),
                ScalarSet {
                    title: Some(format!("t{i}")),
                    ..Default::default()
                },
                Timestamp::now(),
            ),
        )
        .unwrap();
        bead_ids.push(id);
    }

    let mut total = 200;
    while total < 10_000 {
        let i = total % bead_ids.len();
        let id = bead_ids[i].clone();
        let op = make_patch(
            "alice".into(),
            id,
            BTreeMap::new(),
            ScalarSet {
                status: Some(Status::Doing),
                ..Default::default()
            },
            Timestamp::now(),
        );
        let _ = publish::publish_op(&store, &op);
        total += 1;
    }

    let start = Instant::now();
    let state = reducer::replay_store(&store).unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "10k op replay: {}ms over {} accepted beads",
        elapsed.as_millis(),
        state.beads.len()
    );
    assert!(
        elapsed.as_secs() < 30,
        "10k op replay took {}s (target: <30s)",
        elapsed.as_secs()
    );
}
