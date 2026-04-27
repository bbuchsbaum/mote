//! M5 acceptance: A4 (reservation overlap) + A7 (path overlap correctness)
//! plus compound-failure behavior for `mote begin`.

use std::process::Command;
use std::thread;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::op::{ScalarSet, make_create, make_reserve_open};
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
            id,
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
fn reserve_allows_same_actor_overlap() {
    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "auth", "alice");

    let _ = publish::publish_op(
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
    let n2 = publish::publish_op(
        &store,
        &make_reserve_open(
            "alice".into(),
            ids::new_reservation_id(),
            id,
            vec!["src/auth/token.rs".to_string()],
            3600,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let s = reducer::replay_store(&store).unwrap();
    assert!(s.was_accepted(n2.as_str()), "same-actor overlap must be allowed");
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
            .args([
                "begin",
                &id_a,
                "--paths",
                "src/auth/",
                "--actor",
                "alpha",
            ])
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
        stderr.contains("reserve_open rejected") || stderr.contains("path conflict"),
        "expected loser to report reserve_open rejected: {stderr}"
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
    assert_eq!(live.len(), 1, "expected 1 live reservation, got {}", live.len());

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
        .args([
            "begin",
            &id,
            "--paths",
            "src/parser/",
            "--actor",
            "bob",
        ])
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
        .args([
            "reserve",
            "src/auth/",
            "--issue",
            &id,
            "--actor",
            "alice",
        ])
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
    assert!(s2.contains("no live"), "expected 'no live' for unrelated path: {s2}");
}

#[test]
fn cli_smoke_done_compound() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "auth", "alice");
    let bin = mote_bin();

    let _ = Command::new(bin)
        .args([
            "begin",
            &id,
            "--paths",
            "src/auth/",
            "--actor",
            "alice",
        ])
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

// Defensive: confirm scalar-set unused-import warning doesn't actually appear.
#[allow(dead_code)]
fn _unused() -> ScalarSet {
    let _ = make_create("a".into(), "b".into(), ScalarSet::default(), Timestamp::now());
    ScalarSet::default()
}
