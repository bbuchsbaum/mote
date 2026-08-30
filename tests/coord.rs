//! M5 acceptance: A4 (reservation overlap) + A7 (path overlap correctness)
//! plus compound-failure behavior for `mote begin`.

use std::process::Command;
use std::thread;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::op::{
    ScalarSet, make_claim, make_close, make_create, make_release, make_reserve_adopt,
    make_reserve_open,
};
use mote::state::LeaseDisposition;
use mote::{ids, paths, publish, reducer, repo::Store};

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
fn reservation_accepts_human_ttl_and_rejects_overflow() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "human ttl", "alice");
    let out = Command::new(mote_bin())
        .args([
            "reserve",
            "src/ttl.rs",
            "--issue",
            &id,
            "--ttl",
            "2h",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let reservation_id = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert_eq!(
        reducer::replay_store(&store).unwrap().reservations[&reservation_id].ttl_s,
        7200
    );

    let overflow = Command::new(mote_bin())
        .args([
            "reserve",
            "src/overflow.rs",
            "--issue",
            &id,
            "--ttl",
            "4294967295d",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(overflow.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&overflow.stderr).contains("exceeds"));

    let invalid_unit = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "10w", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(invalid_unit.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid_unit.stderr).contains("invalid duration"));
}

#[test]
fn a7_path_overlap_examples() {
    // PRD-listed cases.
    assert!(paths::overlap("src/auth/", "src/auth/token.rs"));
    assert!(paths::overlap("src/auth/", "src/auth/"));
    assert!(!paths::overlap("src/auth/", "src/authn/"));
    assert!(!paths::overlap("src/auth/", "src/authentication/"));
    // Disjoint files in same directory:
    assert!(!paths::overlap("src/auth/token.rs", "src/auth/login.rs"));
}

#[test]
fn reserve_blocks_overlapping_from_other_actor() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "auth", "alice");

    // alice reserves src/auth/
    let v = publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            ids::new_reservation_id(),
            id.clone(),
            vec!["src/auth/".to_string()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let s = reducer::replay_store(&store).unwrap();
    assert!(s.was_accepted(v.as_str()));

    // bob tries to reserve src/auth/token.rs — must be rejected (overlaps alice's dir).
    let bob = publish::publish_op(
        &store,
        &make_reserve_open(
            "bob".into(),
            ids::new_reservation_id(),
            id.clone(),
            vec!["src/auth/token.rs".to_string()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let s2 = reducer::replay_store(&store).unwrap();
    assert!(!s2.was_accepted(bob.as_str()));
    let reason = s2
        .rejection_reason(bob.as_str())
        .expect("must have a reason");
    assert!(
        reason.contains("path conflict"),
        "expected 'path conflict' in reason, got: {reason}"
    );

    // bob reserving an unrelated dir should succeed.
    let bob2 = publish::publish_op(
        &store,
        &make_reserve_open(
            "bob".into(),
            ids::new_reservation_id(),
            id.clone(),
            vec!["src/authn/".to_string()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let s3 = reducer::replay_store(&store).unwrap();
    assert!(s3.was_accepted(bob2.as_str()));
}

#[test]
fn reserve_rejects_same_actor_overlap_with_existing_reservation_diagnostic() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "auth", "alice");

    let first_id = ids::new_reservation_id();
    let _ = publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            first_id.clone(),
            id.clone(),
            vec!["src/auth/".to_string()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let n2 = publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            ids::new_reservation_id(),
            id.clone(),
            vec!["src/auth/token.rs".to_string()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let s = reducer::replay_store(&store).unwrap();
    assert!(!s.was_accepted(n2.as_str()));
    let reason = s.rejection_reason(n2.as_str()).unwrap();
    assert!(reason.contains("duplicate reservation"), "{reason}");
    assert!(reason.contains(&first_id), "{reason}");

    let json = Command::new(mote_bin())
        .args([
            "--json",
            "reserve",
            "src/auth/another.rs",
            "--issue",
            &id,
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value["accepted"], false);
    assert!(value["reason"].as_str().unwrap().contains(&first_id));
}

#[test]
fn concurrent_same_actor_duplicate_attempts_accept_exactly_one() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "same actor race", "alice");
    let bin = mote_bin();
    let dir_a = td.path().to_path_buf();
    let dir_b = td.path().to_path_buf();
    let id_a = id.clone();
    let id_b = id;
    let a = thread::spawn(move || {
        Command::new(bin)
            .args(["reserve", "src/race/", "--issue", &id_a, "--actor", "alice"])
            .current_dir(dir_a)
            .output()
            .unwrap()
    });
    let b = thread::spawn(move || {
        Command::new(bin)
            .args([
                "reserve",
                "src/race/file.rs",
                "--issue",
                &id_b,
                "--actor",
                "alice",
            ])
            .current_dir(dir_b)
            .output()
            .unwrap()
    });
    let a = a.join().unwrap();
    let b = b.join().unwrap();
    assert_ne!(a.status.success(), b.status.success());
    let loser = if a.status.success() { b } else { a };
    assert_eq!(loser.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&loser.stderr).contains("duplicate reservation"));
}

#[test]
fn a4_begin_race_exactly_one_succeeds() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth task", "alice");
    let bin = mote_bin();

    let dir = td.path().to_path_buf();
    let id_a = id.clone();
    let id_b = id.clone();

    let h1 = thread::spawn(move || {
        Command::new(bin)
            .args(["begin", &id_a, "--paths", "src/auth/", "--actor", "alpha"])
            .current_dir(&dir)
            .output()
            .unwrap()
    });
    let dir2 = td.path().to_path_buf();
    let h2 = thread::spawn(move || {
        Command::new(bin)
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
        "exactly one begin must succeed: a.success={} b.success={} a.stderr={} b.stderr={}",
        s1,
        s2,
        String::from_utf8_lossy(&r1.stderr),
        String::from_utf8_lossy(&r2.stderr),
    );
    let loser = if s1 { r2 } else { r1 };
    assert_eq!(loser.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&loser.stderr);
    assert!(
        stderr.contains("reserve_open rejected")
            || stderr.contains("path conflict")
            || stderr.contains("claim rejected"),
        "expected loser to report a reservation or claim race loss: {stderr}"
    );

    // Replay: exactly one live reservation, exactly one claim.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());
    let live: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.is_active(&now))
        .collect();
    assert_eq!(
        live.len(),
        1,
        "expected 1 live reservation, got {}",
        live.len()
    );

    let bead = &state.beads[&id];
    assert!(bead.claim.is_some(), "winner must have published a claim");
}

#[test]
fn begin_compensates_when_claim_fails() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "auth", "alice");

    // Pre-claim the bead by alice so a different actor's begin will fail
    // at the claim step (despite being able to reserve a free path).
    let bin = mote_bin();
    let claim = Command::new(bin)
        .args(["claim", &id, "--ttl", "3600", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(claim.status.success());

    // bob begins on a free path. reserve should succeed; claim should fail
    // (alice still holds it). bob's reserve_open must be compensated.
    let begin = Command::new(bin)
        .args(["begin", &id, "--paths", "src/parser/", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(!begin.status.success(), "bob's begin should fail");
    assert_eq!(begin.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&begin.stderr);
    assert!(
        stderr.contains("claim rejected"),
        "expected 'claim rejected' in stderr: {stderr}"
    );

    // Replay: bob's reservation must NOT be live (compensated).
    let state = reducer::replay_store(&store).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());
    let live_for_bob: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.actor == "bob" && r.is_active(&now))
        .collect();
    assert_eq!(
        live_for_bob.len(),
        0,
        "bob's reservation must be compensated (closed) after claim failure"
    );

    // Claim still belongs to alice.
    let bead = &state.beads[&id];
    let c = bead.claim.as_ref().expect("alice's claim must remain");
    assert_eq!(c.claimed_by, "alice");
}

#[test]
fn begin_marks_open_work_doing_and_removes_it_from_ready() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "auth", "alice");
    let bin = mote_bin();

    let ready_before = Command::new(bin)
        .args(["ready", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ready_before.status.success());
    let ready_before_s = String::from_utf8(ready_before.stdout).unwrap();
    assert!(
        ready_before_s.contains(&id),
        "new open bead should be ready before begin:\n{ready_before_s}"
    );

    let begin = Command::new(bin)
        .args([
            "begin",
            &id,
            "--paths",
            "src/auth/",
            "--note",
            "starting",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        begin.status.success(),
        "begin failed: stderr={}",
        String::from_utf8_lossy(&begin.stderr)
    );

    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    assert_eq!(bead.status.as_str(), "doing");
    assert!(bead.claim.is_some(), "begin should still claim the bead");

    let ready_after = Command::new(bin)
        .args(["ready", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(ready_after.status.success());
    let ready_after_s = String::from_utf8(ready_after.stdout).unwrap();
    assert!(
        !ready_after_s.contains(&id),
        "begun bead should not stay in ready:\n{ready_after_s}"
    );
}

#[test]
fn preflight_reports_overlaps_and_clear_paths() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth", "alice");
    let bin = mote_bin();

    // alice reserves src/auth/
    let _ = Command::new(bin)
        .args([
            "reserve",
            "src/auth/",
            "--issue",
            &id,
            "--ttl",
            "3600",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();

    // bob preflights overlapping path → exit 2.
    let pf = Command::new(bin)
        .args([
            "preflight",
            "--issue",
            &id,
            "--paths",
            "src/auth/token.rs",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(pf.status.code(), Some(2));
    let s = String::from_utf8(pf.stdout).unwrap();
    assert!(s.contains("conflicts:"), "expected conflicts in: {s}");

    // bob preflights disjoint path → exit 0.
    let pf2 = Command::new(bin)
        .args([
            "preflight",
            "--issue",
            &id,
            "--paths",
            "src/parser/",
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(pf2.status.code(), Some(0));
    let s2 = String::from_utf8(pf2.stdout).unwrap();
    assert!(s2.contains("clear"), "expected 'clear' in: {s2}");
}

#[test]
fn who_has_finds_overlapping_holders() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth", "alice");
    let bin = mote_bin();

    let _ = Command::new(bin)
        .args(["reserve", "src/auth/", "--issue", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();

    let wh = Command::new(bin)
        .args(["who-has", "src/auth/token.rs"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(wh.status.success());
    let s = String::from_utf8(wh.stdout).unwrap();
    assert!(s.contains("alice"), "expected alice in who-has output: {s}");
    assert!(s.contains("src/auth/"), "expected held path in: {s}");

    let wh2 = Command::new(bin)
        .args(["who-has", "src/parser/main.rs"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s2 = String::from_utf8(wh2.stdout).unwrap();
    assert!(
        s2.contains("no live"),
        "expected 'no live' for unrelated path: {s2}"
    );
}

#[test]
fn cli_smoke_done_compound() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth", "alice");
    let bin = mote_bin();

    let _ = Command::new(bin)
        .args(["begin", &id, "--paths", "src/auth/", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();

    let done = Command::new(bin)
        .args(["done", &id, "--note", "shipped", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        done.status.success(),
        "done failed: stderr={}",
        String::from_utf8_lossy(&done.stderr)
    );

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    assert_eq!(bead.status.as_str(), "closed");
    assert!(bead.claim.is_none(), "claim should be released");

    let now = ids::format_rfc3339(Timestamp::now());
    let live_alice: Vec<_> = state
        .reservations
        .values()
        .filter(|r| r.actor == "alice" && r.is_active(&now))
        .collect();
    assert!(live_alice.is_empty(), "all reservations should be closed");
}

#[test]
fn orphaned_reservation_is_visible_blocking_and_adoptable_with_provenance() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let source = new_bead(&td, "source", "alice");
    let target = new_bead(&td, "target", "bob");
    let bin = mote_bin();

    let reserve = Command::new(bin)
        .args([
            "reserve",
            "src/shared/",
            "--issue",
            &source,
            "--ttl",
            "3600",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(reserve.status.success());
    let reservation_id = String::from_utf8(reserve.stdout)
        .unwrap()
        .trim()
        .to_string();

    let claim = Command::new(bin)
        .args(["claim", &target, "--ttl", "3600", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(claim.status.success());

    let live_takeover = Command::new(bin)
        .args([
            "adopt",
            &reservation_id,
            "--issue",
            &target,
            "--actor",
            "bob",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(live_takeover.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&live_takeover.stderr).contains("only a still-live orphaned"));

    let close = Command::new(bin)
        .args(["close", &source, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(close.status.success());

    let state = reducer::replay_store(&store).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());
    let orphan = &state.reservations[&reservation_id];
    assert_eq!(
        state.reservation_disposition(orphan, &now),
        LeaseDisposition::Orphaned
    );

    let preflight = Command::new(bin)
        .args([
            "preflight",
            "--issue",
            &target,
            "--paths",
            "src/shared/file.rs",
            "--actor",
            "carol",
            "--json",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(preflight.status.code(), Some(2));
    let preflight_json: serde_json::Value = serde_json::from_slice(&preflight.stdout).unwrap();
    assert_eq!(preflight_json["conflicts"][0]["disposition"], "orphaned");

    let unclaimed = Command::new(bin)
        .args([
            "adopt",
            &reservation_id,
            "--issue",
            &target,
            "--actor",
            "carol",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert_eq!(unclaimed.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&unclaimed.stderr).contains("must hold a live claim"));

    let adopt = Command::new(bin)
        .args([
            "adopt",
            &reservation_id,
            "--issue",
            &target,
            "--actor",
            "bob",
            "--json",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        adopt.status.success(),
        "adopt failed: {}",
        String::from_utf8_lossy(&adopt.stderr)
    );
    let adopted_json: serde_json::Value = serde_json::from_slice(&adopt.stdout).unwrap();
    assert_eq!(adopted_json["actor"], "bob");
    assert_eq!(adopted_json["entity"], target);
    assert_eq!(adopted_json["disposition"], "active");
    assert_eq!(adopted_json["adoptions"][0]["from_actor"], "alice");
    assert_eq!(adopted_json["adoptions"][0]["from_entity"], source);

    let state = reducer::replay_store(&store).unwrap();
    let adopted = &state.reservations[&reservation_id];
    assert_eq!(adopted.live_paths(), vec!["src/shared/".to_string()]);
    assert_eq!(adopted.adoptions.len(), 1);
    assert_eq!(adopted.clock, adopted.adoptions[0].op_id);

    let events = Command::new(bin)
        .args(["events", "--kind", "all", "--json"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(events.status.success());
    let event_lines = String::from_utf8(events.stdout).unwrap();
    let values: Vec<serde_json::Value> = event_lines
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(values.iter().any(|value| {
        value["type"] == "reservation.adopted" && value["data"]["reservation_id"] == reservation_id
    }));
    let close_event = values
        .iter()
        .find(|value| value["type"] == "issue.closed" && value["data"]["entity"] == source)
        .expect("source close event");
    assert_eq!(
        close_event["data"]["lease_effects"]["orphaned_reservations"][0]["reservation_id"],
        reservation_id
    );
}

#[test]
fn orphan_adoption_cas_race_accepts_exactly_one() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let source = new_bead(&td, "source", "alice");
    let target_bob = new_bead(&td, "bob target", "bob");
    let target_carol = new_bead(&td, "carol target", "carol");
    let now = Timestamp::now();

    let reserve_id = ids::new_reservation_id();
    let opened = publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            reserve_id.clone(),
            source.clone(),
            vec!["src/race.rs".into()],
            3600,
            now,
        ),
    )
    .unwrap();
    for (actor, target) in [("bob", &target_bob), ("carol", &target_carol)] {
        publish::publish_op(
            &store,
            &make_claim(actor.into(), target.clone(), actor.into(), 3600, None, now),
        )
        .unwrap();
    }
    publish::publish_op(
        &store,
        &make_close("alice".into(), source, Default::default(), now),
    )
    .unwrap();

    let a = publish::publish_op(
        &store,
        &make_reserve_adopt(
            "bob".into(),
            reserve_id.clone(),
            target_bob,
            opened.as_str().into(),
            3600,
            now,
        ),
    )
    .unwrap();
    let b = publish::publish_op(
        &store,
        &make_reserve_adopt(
            "carol".into(),
            reserve_id.clone(),
            target_carol,
            opened.as_str().into(),
            3600,
            now,
        ),
    )
    .unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert_ne!(
        state.was_accepted(a.as_str()),
        state.was_accepted(b.as_str())
    );
    let loser = if state.was_accepted(a.as_str()) {
        &b
    } else {
        &a
    };
    assert!(
        state
            .rejection_reason(loser.as_str())
            .unwrap()
            .contains("stale reservation CAS")
    );
    assert_eq!(state.reservations[&reserve_id].adoptions.len(), 1);
    let replayed = reducer::replay_store(&store).unwrap();
    assert_eq!(
        replayed.reservations[&reserve_id].clock,
        state.reservations[&reserve_id].clock
    );
    assert_eq!(
        serde_json::to_value(&replayed.reservations[&reserve_id].adoptions).unwrap(),
        serde_json::to_value(&state.reservations[&reserve_id].adoptions).unwrap()
    );
}

#[test]
fn expired_orphan_cannot_be_adopted_and_closed_claim_cannot_be_renewed() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let source = ids::new_bead_id();
    let target = ids::new_bead_id();
    let t_create: Timestamp = "2025-12-31T23:59:59Z".parse().unwrap();
    let t0: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
    let t1: Timestamp = "2026-01-01T00:00:01Z".parse().unwrap();
    let t3: Timestamp = "2026-01-01T00:00:03Z".parse().unwrap();

    for (actor, id, title) in [("alice", &source, "source"), ("bob", &target, "target")] {
        publish::publish_op(
            &store,
            &make_create(
                actor.into(),
                id.clone(),
                ScalarSet {
                    title: Some(title.into()),
                    ..Default::default()
                },
                t_create,
            ),
        )
        .unwrap();
    }

    let reservation_id = ids::new_reservation_id();
    let opened = publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            reservation_id.clone(),
            source.clone(),
            vec!["src/expired.rs".into()],
            2,
            t0,
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_claim(
            "alice".into(),
            source.clone(),
            "alice".into(),
            3600,
            None,
            t0,
        ),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_claim("bob".into(), target.clone(), "bob".into(), 3600, None, t0),
    )
    .unwrap();
    publish::publish_op(
        &store,
        &make_close("alice".into(), source.clone(), Default::default(), t1),
    )
    .unwrap();

    let expired = publish::publish_op(
        &store,
        &make_reserve_adopt(
            "bob".into(),
            reservation_id.clone(),
            target,
            opened.as_str().into(),
            3600,
            t3,
        ),
    )
    .unwrap();
    let renew = publish::publish_op(
        &store,
        &make_claim(
            "alice".into(),
            source.clone(),
            "alice".into(),
            3600,
            None,
            t1,
        ),
    )
    .unwrap();
    let release = publish::publish_op(
        &store,
        &make_release("alice".into(), source.clone(), None, t1),
    )
    .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(expired.as_str()));
    let expired_reason = state.rejection_reason(expired.as_str()).unwrap();
    assert!(
        expired_reason.contains("only a still-live orphaned"),
        "unexpected rejection: {expired_reason}"
    );
    assert!(!state.was_accepted(renew.as_str()));
    assert!(
        state
            .rejection_reason(renew.as_str())
            .unwrap()
            .contains("cannot claim or renew closed work")
    );
    assert!(state.was_accepted(release.as_str()));
    assert!(state.beads[&source].claim.is_none());
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&reservation_id],
            &ids::format_rfc3339(t3)
        ),
        LeaseDisposition::Expired
    );
}

// Defensive: confirm scalar-set unused-import warning doesn't actually appear.
#[allow(dead_code)]
fn _unused() -> ScalarSet {
    let _ = make_create(
        "a".into(),
        "b".into(),
        ScalarSet::default(),
        Timestamp::now(),
    );
    ScalarSet::default()
}
