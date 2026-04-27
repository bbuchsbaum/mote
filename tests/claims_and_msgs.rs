//! M4 acceptance: A5 (claim lease expiry) + A6 (msg send/inbox/ack).

use std::process::Command;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

use mote::{reducer, repo::Store};

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
    assert!(
        out.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[test]
fn a5_claim_lease_expiry_yields_to_next_actor() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "shared task", "alice");

    // alice claims with ttl=1 second.
    let claim_a = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "1", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        claim_a.status.success(),
        "alice's claim failed: stderr={}",
        String::from_utf8_lossy(&claim_a.stderr)
    );

    // While alice's lease is still live, bob's claim must be rejected (exit 2).
    let bob_now = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        !bob_now.status.success(),
        "bob should be rejected while alice's lease is live"
    );
    assert_eq!(bob_now.status.code(), Some(2));

    // Wait past the lease.
    thread::sleep(Duration::from_secs(2));

    // bob's claim now succeeds.
    let bob_later = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        bob_later.status.success(),
        "bob's claim after lease expiry failed: stderr={}",
        String::from_utf8_lossy(&bob_later.stderr)
    );

    // Replay: claim now belongs to bob.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    let claim = bead.claim.as_ref().expect("bead must have a claim");
    assert_eq!(claim.claimed_by, "bob");
}

#[test]
fn a6_msg_send_inbox_ack_round_trip() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let issue = new_bead(&td, "auth bug", "alice");
    let bin = mote_bin();

    // alice → bob
    let send_out = Command::new(bin)
        .args([
            "msg",
            "send",
            "--to",
            "bob",
            "--issue",
            &issue,
            "--kind",
            "request",
            "Please take tests",
            "--actor",
            "alice",
        ])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        send_out.status.success(),
        "msg send failed: stderr={}",
        String::from_utf8_lossy(&send_out.stderr)
    );
    let msg_id = String::from_utf8(send_out.stdout).unwrap().trim().to_string();
    assert!(msg_id.starts_with("msg-"), "expected msg-... got {msg_id}");

    // bob inbox lists the message.
    let inbox1 = Command::new(bin)
        .args(["inbox", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(inbox1.status.success());
    let s1 = String::from_utf8(inbox1.stdout).unwrap();
    assert!(s1.contains(&msg_id), "expected msg_id in bob's inbox: {s1}");
    assert!(s1.contains("Please take tests"));

    // alice's inbox does not list it (she's the sender, not recipient).
    let inbox_alice = Command::new(bin)
        .args(["inbox", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let sa = String::from_utf8(inbox_alice.stdout).unwrap();
    assert!(!sa.contains(&msg_id), "alice should not see her own send: {sa}");

    // self-ack by alice (the sender) must fail.
    let bad_ack = Command::new(bin)
        .args(["msg", "ack", &msg_id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        !bad_ack.status.success(),
        "self-ack must be rejected; got success"
    );

    // bob acks.
    let ack = Command::new(bin)
        .args(["msg", "ack", &msg_id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        ack.status.success(),
        "bob ack failed: stderr={}",
        String::from_utf8_lossy(&ack.stderr)
    );

    // bob's inbox is now empty for that msg.
    let inbox2 = Command::new(bin)
        .args(["inbox", "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    let s2 = String::from_utf8(inbox2.stdout).unwrap();
    assert!(
        !s2.contains(&msg_id),
        "bob's inbox should not contain acked msg: {s2}"
    );

    // double ack by bob is rejected.
    let dup = Command::new(bin)
        .args(["msg", "ack", &msg_id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(!dup.status.success(), "duplicate ack must fail");

    // Library-side verification: alice's view of the message has ack info.
    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let rec = state.messages.get(&msg_id).expect("msg must exist");
    assert_eq!(rec.from, "alice");
    assert_eq!(rec.to, "bob");
    assert_eq!(rec.entity.as_deref(), Some(issue.as_str()));
    assert!(rec.ack_op_id.is_some(), "msg must be acked");
    assert!(rec.ack_ts.is_some());

    // Issue history surfaces both the send and the ack as accepted entries.
    let entries = state.history.get(&issue).expect("issue must have history");
    let kinds_accepted: Vec<&str> = entries
        .iter()
        .filter(|e| e.accepted)
        .map(|e| e.kind.as_str())
        .collect();
    assert!(
        kinds_accepted.contains(&"msg_send"),
        "expected msg_send in accepted history: {kinds_accepted:?}"
    );
    assert!(
        kinds_accepted.contains(&"msg_ack"),
        "expected msg_ack in accepted history: {kinds_accepted:?}"
    );
}

#[test]
fn claim_release_round_trip_via_cli() {
    let td = TempDir::new().unwrap();
    init_store(&td);
    let id = new_bead(&td, "task", "alice");

    let claim = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(claim.status.success());

    // Same actor re-claim should succeed (auto-fills expect_claim).
    let renew = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "60", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        renew.status.success(),
        "alice should be able to renew her own claim"
    );

    // Different actor's claim is rejected while lease is live.
    let foreign = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(!foreign.status.success());
    assert_eq!(foreign.status.code(), Some(2));

    // alice releases.
    let release = Command::new(mote_bin())
        .args(["release", &id, "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(
        release.status.success(),
        "release failed: stderr={}",
        String::from_utf8_lossy(&release.stderr)
    );

    // Now bob can claim.
    let bob_claim = Command::new(mote_bin())
        .args(["claim", &id, "--actor", "bob"])
        .current_dir(td.path())
        .output()
        .unwrap();
    assert!(bob_claim.status.success());

    let store = Store::open(&td.path().join(".mote")).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    let claim_state = state.beads[&id]
        .claim
        .as_ref()
        .expect("must have claim");
    assert_eq!(claim_state.claimed_by, "bob");
}

#[test]
fn ready_excludes_foreign_claimed() {
    use jiff::Timestamp;
    use mote::ids;

    let td = TempDir::new().unwrap();
    let store = init_store(&td);
    let id = new_bead(&td, "ready test", "alice");

    // Alice claims it.
    let _ = Command::new(mote_bin())
        .args(["claim", &id, "--ttl", "60", "--actor", "alice"])
        .current_dir(td.path())
        .output()
        .unwrap();

    let state = reducer::replay_store(&store).unwrap();
    let now = ids::format_rfc3339(Timestamp::now());

    // is_ready alone says yes (status=open, no deps).
    assert!(state.is_ready(&state.beads[&id]));

    // ready_beads_for("bob", now) excludes the bead.
    let bob_ready: Vec<&str> = state
        .ready_beads_for("bob", &now)
        .map(|b| b.id.as_str())
        .collect();
    assert!(!bob_ready.contains(&id.as_str()), "bob should not see foreign-claimed bead");

    // ready_beads_for("alice", now) still includes it (her own claim).
    let alice_ready: Vec<&str> = state
        .ready_beads_for("alice", &now)
        .map(|b| b.id.as_str())
        .collect();
    assert!(alice_ready.contains(&id.as_str()));
}
