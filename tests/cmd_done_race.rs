//! Deterministic CLI race test for `mote done`.
//!
//! Background: `cmd_done` reads bead state, builds an `expect.status` clock,
//! and publishes a `close` op. If a competing writer changes status between
//! the read and the publish, the close's expect is stale and the reducer
//! rejects it — `cmd_done` MUST exit 2.
//!
//! The natural wall-clock window is microseconds. We widen it via the
//! `MOTE_TEST_DELAY_BEFORE_CLOSE_MS` env var hook in `cmd_done` so the race
//! is deterministic.

use std::process::Command;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

use mote::{reducer, repo::Store};

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn init_store(td: &TempDir) {
    let out = Command::new(mote_bin())
        .arg("init")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed");
}

fn new_bead(td: &TempDir, title: &str, actor: &str) -> String {
    let out = Command::new(mote_bin())
        .args(["new", title, "--actor", actor])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn cmd_done_loses_close_race_to_concurrent_writer() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "task", "alice");

    let dir = td.path().to_path_buf();
    let id_for_done = id.clone();
    let bin = mote_bin();

    // Spawn alice's `done` with a 600ms hook before close, so we can race a
    // status patch into the window deterministically.
    let alice = thread::spawn(move || {
        Command::new(bin)
            .args(["done", &id_for_done, "--actor", "alice"])
            .env("MOTE_TEST_DELAY_BEFORE_CLOSE_MS", "600")
            .current_dir(&dir)
            .output()
            .unwrap()
    });

    // Wait long enough for alice to enter the sleep, then bob mutates status.
    thread::sleep(Duration::from_millis(200));
    let bob = Command::new(bin)
        .args(["set", &id, "status=blocked", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        bob.status.success(),
        "bob's set should succeed: {}",
        String::from_utf8_lossy(&bob.stderr)
    );

    let alice_out = alice.join().unwrap();
    assert!(
        !alice_out.status.success(),
        "alice's done should NOT exit 0 when its close is rejected; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&alice_out.stdout),
        String::from_utf8_lossy(&alice_out.stderr),
    );
    assert_eq!(
        alice_out.status.code(),
        Some(2),
        "alice's done must exit 2 on close rejection (got {:?})",
        alice_out.status.code()
    );
    let stderr = String::from_utf8_lossy(&alice_out.stderr);
    assert!(
        stderr.contains("close rejected"),
        "expected `close rejected` in alice's stderr, got: {stderr}"
    );

    // Bead state must reflect bob's update, NOT a closed bead.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    assert_eq!(
        bead.status.as_str(),
        "blocked",
        "alice's failed done must not have closed the bead; status={}",
        bead.status.as_str()
    );
}

#[test]
fn cmd_done_succeeds_in_calm_window() {
    // Sanity: when no one else writes, done's race window does not produce a
    // false reject. With the hook set, alice still wins because no competing
    // writer arrives.
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "task", "alice");

    let out = Command::new(mote_bin())
        .args(["done", &id, "--actor", "alice"])
        .env("MOTE_TEST_DELAY_BEFORE_CLOSE_MS", "100")
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "done should succeed when uncontested: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&id].status.as_str(), "closed");
}
