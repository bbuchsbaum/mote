//! M6 acceptance: A10 — replay determinism.
//!
//! Build a fixed `ops/` directory exercising every op kind, then replay it
//! twice in fresh state objects. The Debug representation of `State`
//! (deterministic by construction — every container is a BTreeMap or insertion-
//! ordered Vec, every value type is fully owned) must match byte-for-byte.

use std::collections::BTreeMap;
use std::process::Command;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::ids;
use mote::op::{
    ScalarSet, Status, make_claim, make_close, make_create, make_dep, make_msg_ack, make_msg_send,
    make_note, make_patch, make_release, make_reserve_close, make_reserve_open, make_tag,
};
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
    assert!(out.status.success());
    Store::open(&td.path().join(".mote")).unwrap()
}

#[test]
fn a10_two_replays_produce_identical_state() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);

    // Two beads.
    let a = "bd-A-fixture".to_string();
    let b = "bd-B-fixture".to_string();
    publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            a.clone(),
            ScalarSet {
                title: Some("Alpha".into()),
                priority: Some(1),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_create(
            "alice".into(),
            b.clone(),
            ScalarSet {
                title: Some("Beta".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();

    // patch + tag + dep on A
    let s = reducer::replay_store(&store).unwrap();
    let mut expect = BTreeMap::new();
    expect.insert("status".into(), s.beads[&a].clock["status"].clone());
    publish::publish_op(
        &store,
        &make_patch(
            "alice".into(),
            a.clone(),
            expect,
            ScalarSet {
                status: Some(Status::Doing),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_tag(true, "alice".into(), a.clone(), "backend".into(), Timestamp::now()),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_dep(
            true,
            "alice".into(),
            a.clone(),
            b.clone(),
            "blocks".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();

    // notes on A
    publish::publish_op(
        &store,
        &make_note(
            "alice".into(),
            a.clone(),
            "progress".into(),
            "starting work".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_note(
            "bob".into(),
            a.clone(),
            "decision".into(),
            "moving forward with approach X".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();

    // claim + release on A
    publish::publish_op(
        &store,
        &make_claim(
            "alice".into(),
            a.clone(),
            "alice".into(),
            3600,
            None,
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_release("alice".into(), a.clone(), None, Timestamp::now()),
    )
    .unwrap();

    // msg_send + msg_ack
    let msg_id = ids::new_msg_id();
    publish::publish_op(
        &store,
        &make_msg_send(
            "alice".into(),
            msg_id.clone(),
            "bob".into(),
            Some(a.clone()),
            None,
            "request".into(),
            "please review".into(),
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_msg_ack("bob".into(), msg_id, Timestamp::now()),
    )
    .unwrap();

    // reserve_open + partial reserve_close
    let rv = ids::new_reservation_id();
    publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            rv.clone(),
            a.clone(),
            vec!["src/auth/".into(), "tests/auth/".into()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_reserve_close(
            "alice".into(),
            rv,
            Some(vec!["tests/auth/".into()]),
            Timestamp::now(),
        ),
    )
    .unwrap();

    // close on B (idempotent semantics)
    publish::publish_op(
        &store,
        &make_close("alice".into(), b.clone(), BTreeMap::new(), Timestamp::now()),
    )
    .unwrap();

    // Also exercise a rejected op so history carries a REJECT entry.
    let stale_clock = "20200101T000000.000000Z-p0001-c0000-r0000-h000000".to_string();
    let mut bad_expect = BTreeMap::new();
    bad_expect.insert("title".into(), stale_clock);
    publish::publish_op(
        &store,
        &make_patch(
            "carol".into(),
            a.clone(),
            bad_expect,
            ScalarSet {
                title: Some("Should not stick".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    )
    .unwrap();

    // First replay.
    let s1 = reducer::replay_store(&store).unwrap();
    // Second replay.
    let s2 = reducer::replay_store(&store).unwrap();

    let dbg1 = format!("{s1:#?}");
    let dbg2 = format!("{s2:#?}");
    assert_eq!(
        dbg1, dbg2,
        "two replays must produce byte-identical state Debug representations"
    );

    // Sanity: high-level invariants are stable.
    assert_eq!(s1.beads.len(), s2.beads.len());
    assert_eq!(s1.history.len(), s2.history.len());
    assert_eq!(s1.messages.len(), s2.messages.len());
    assert_eq!(s1.reservations.len(), s2.reservations.len());
    assert_eq!(s1.beads[&a].notes.len(), s2.beads[&a].notes.len());

    // Bead A: status doing; tags={backend}; deps={(B,blocks)}; 2 notes; closed claim absent.
    let a1 = &s1.beads[&a];
    assert_eq!(a1.status, Status::Doing);
    assert!(a1.tags.contains("backend"));
    assert_eq!(a1.deps.len(), 1);
    assert_eq!(a1.notes.len(), 2);
    assert!(a1.claim.is_none(), "released claim should be cleared");

    // Bead B: status closed.
    assert_eq!(s1.beads[&b].status, Status::Closed);

    // Reservation: live with one path closed, one open.
    let rv_state = s1.reservations.values().next().unwrap();
    assert_eq!(rv_state.paths.len(), 2);
    assert_eq!(rv_state.closed_paths.len(), 1);

    // History on bead A includes our REJECT entry.
    let history_a = &s1.history[&a];
    let rejects: Vec<&str> = history_a
        .iter()
        .filter(|e| !e.accepted)
        .map(|e| e.kind.as_str())
        .collect();
    assert!(rejects.contains(&"patch"), "expected stale patch in history: {rejects:?}");
}
