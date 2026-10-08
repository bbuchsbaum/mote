//! Claim lifecycle for unattended worker loops: stranded `doing` work returns
//! to `ready`, same-actor sessions cannot silently share a claim, `next`
//! claims atomically, and `begin`/`done` respect who holds the work.

use std::path::Path;
use std::process::{Command, Output};

use jiff::{SignedDuration, Timestamp};
use tempfile::TempDir;

use mote::{ids, reducer, repo::Store};

fn mote_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mote")
}

fn mote(dir: &Path, args: &[&str]) -> Output {
    Command::new(mote_bin())
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

fn init(td: &TempDir) -> Store {
    ok(&mote(td.path(), &["init"]));
    Store::open(&td.path().join(".mote")).unwrap()
}

fn new_bead(td: &TempDir, title: &str, priority: &str) -> String {
    let id = ok(&mote(
        td.path(),
        &["new", title, "--priority", priority, "--actor", "alice"],
    ));
    id.trim().to_string()
}

fn ready_json(td: &TempDir, actor: &str) -> Vec<serde_json::Value> {
    let out = ok(&mote(td.path(), &["--json", "ready", "--actor", actor]));
    serde_json::from_str(&out).unwrap()
}

#[test]
fn released_doing_bead_returns_to_ready_for_other_actors() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "stranded by release", "1");

    ok(&mote(td.path(), &["claim", &id, "--actor", "alice"]));
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "alice"],
    ));
    assert!(
        ready_json(&td, "bob")
            .iter()
            .all(|b| b["id"] != id.as_str()),
        "a live claim keeps the bead out of bob's queue"
    );

    ok(&mote(td.path(), &["release", &id, "--actor", "alice"]));
    let ready = ready_json(&td, "bob");
    let entry = ready
        .iter()
        .find(|b| b["id"] == id.as_str())
        .expect("released doing bead must be offered again");
    assert_eq!(entry["status"], "doing");
    assert_eq!(entry["stranded"]["reason"], "released");
    assert_eq!(entry["stranded"]["last_holder"], "alice");
    assert!(entry["stranded"]["since_ts"].is_string());

    let text = ok(&mote(td.path(), &["ready", "--actor", "bob"]));
    assert!(text.contains("[stranded: released by alice"), "{text}");

    // Bob can pick it up; once he holds it, it is no longer stranded.
    ok(&mote(td.path(), &["claim", &id, "--actor", "bob"]));
    assert!(
        ready_json(&td, "carol")
            .iter()
            .all(|b| b["id"] != id.as_str())
    );
    let mine = ready_json(&td, "bob");
    assert!(
        mine.iter().all(|b| b["id"] != id.as_str()),
        "a doing bead held by the caller is in progress, not ready"
    );
}

#[test]
fn expired_doing_claim_is_offered_with_expiry_provenance() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "stranded by expiry", "2");
    ok(&mote(
        td.path(),
        &["claim", &id, "--ttl", "60", "--actor", "alice"],
    ));
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "alice"],
    ));

    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    let lease = bead.claim.as_ref().unwrap().lease_until_ts.clone();
    let now = ids::format_rfc3339(Timestamp::now());
    assert!(state.stranded(bead, &now).is_none());

    let after: Timestamp = lease.parse().unwrap();
    let after = ids::format_rfc3339(after.checked_add(SignedDuration::from_secs(1)).unwrap());
    let st = state
        .stranded(bead, &after)
        .expect("expired claim strands the bead");
    assert_eq!(st.reason.as_str(), "expired");
    assert_eq!(st.last_holder.as_deref(), Some("alice"));
    assert_eq!(st.since_ts.as_deref(), Some(lease.as_str()));
    let ready: Vec<&str> = state
        .ready_beads_for("bob", &after)
        .map(|b| b.id.as_str())
        .collect();
    assert!(ready.contains(&id.as_str()));
}

#[test]
fn expired_doing_claim_is_claimable_from_ready_end_to_end() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "short lease", "1");
    ok(&mote(
        td.path(),
        &["claim", &id, "--ttl", "1", "--actor", "alice"],
    ));
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "alice"],
    ));
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let ready = ready_json(&td, "bob");
    let entry = ready
        .iter()
        .find(|b| b["id"] == id.as_str())
        .expect("expired doing bead must be offered again");
    assert_eq!(entry["stranded"]["reason"], "expired");
    ok(&mote(td.path(), &["claim", &id, "--actor", "bob"]));

    let board: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "board", "--actor", "bob"],
    )))
    .unwrap();
    assert!(board["stranded"].as_array().unwrap().is_empty());
}

#[test]
fn stranded_beads_are_surfaced_in_board_in_flight_and_doctor() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "visible stranding", "1");
    ok(&mote(td.path(), &["claim", &id, "--actor", "alice"]));
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "alice"],
    ));
    ok(&mote(td.path(), &["release", &id, "--actor", "alice"]));

    let board: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "board", "--actor", "bob"],
    )))
    .unwrap();
    let stranded = board["stranded"].as_array().unwrap();
    assert_eq!(stranded.len(), 1);
    assert_eq!(stranded[0]["id"], id.as_str());
    assert_eq!(stranded[0]["stranded"]["reason"], "released");
    let text = ok(&mote(td.path(), &["board", "--actor", "bob"]));
    assert!(text.contains(&format!("STRANDED {id}")), "{text}");

    let flight: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "in-flight", "--actor", "bob"],
    )))
    .unwrap();
    let doing = flight["doing"].as_array().unwrap();
    assert_eq!(doing[0]["stranded"]["last_holder"], "alice");

    let doctor: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "doctor", "--actor", "bob"],
    )))
    .unwrap();
    let warnings = doctor["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains(&id) && w.as_str().unwrap().contains("doing")),
        "{warnings:?}"
    );
}

#[test]
fn blocked_doing_bead_is_not_stranded() {
    let td = TempDir::new().unwrap();
    init(&td);
    let parent = new_bead(&td, "parent", "1");
    let child = new_bead(&td, "child", "1");
    ok(&mote(
        td.path(),
        &["dep", "add", &child, &parent, "--actor", "alice"],
    ));
    ok(&mote(
        td.path(),
        &["set", &child, "status=doing", "--actor", "alice"],
    ));
    assert!(
        ready_json(&td, "bob")
            .iter()
            .all(|b| b["id"] != child.as_str())
    );
}

#[test]
fn begin_without_paths_claims_and_starts_without_a_reservation() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "no files", "1");
    let out = mote(
        td.path(),
        &["begin", &id, "--note", "starting", "--actor", "alice"],
    );
    let stdout = ok(&out);
    assert!(
        stdout.trim().is_empty(),
        "no reservation id to print: {stdout}"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("no paths reserved"));

    let state = reducer::replay_store(&store).unwrap();
    let bead = &state.beads[&id];
    assert_eq!(bead.status.as_str(), "doing");
    assert_eq!(bead.claim.as_ref().unwrap().claimed_by, "alice");
    assert!(state.reservations.values().all(|r| r.entity != id));

    // A competing pathless begin is still fenced by the claim.
    let bob = mote(td.path(), &["begin", &id, "--actor", "bob"]);
    assert_eq!(bob.status.code(), Some(2));
}

#[test]
fn done_refuses_to_close_another_actors_live_claim_without_force() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "alice's work", "1");
    ok(&mote(
        td.path(),
        &["begin", &id, "--paths", "src/a.rs", "--actor", "alice"],
    ));
    let before = store.list_op_filenames().unwrap().len();

    let bob = mote(td.path(), &["done", &id, "--actor", "bob"]);
    assert_eq!(bob.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&bob.stderr);
    assert!(
        stderr.contains("claimed by alice") && stderr.contains("--force"),
        "{stderr}"
    );
    assert_eq!(
        store.list_op_filenames().unwrap().len(),
        before,
        "a refused done publishes nothing"
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&id].status.as_str(), "doing");

    let forced = mote(td.path(), &["done", &id, "--force", "--actor", "bob"]);
    ok(&forced);
    assert!(String::from_utf8_lossy(&forced.stderr).contains("--force"));
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&id].status.as_str(), "closed");
}

#[test]
fn done_by_holder_or_on_unclaimed_bead_still_succeeds() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let held = new_bead(&td, "held", "1");
    let loose = new_bead(&td, "unclaimed", "1");
    ok(&mote(td.path(), &["begin", &held, "--actor", "alice"]));
    ok(&mote(td.path(), &["done", &held, "--actor", "alice"]));
    ok(&mote(td.path(), &["done", &loose, "--actor", "bob"]));
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&held].status.as_str(), "closed");
    assert!(state.beads[&held].claim.is_none());
    assert_eq!(state.beads[&loose].status.as_str(), "closed");
}

fn start_session(td: &TempDir, actor: &str) -> String {
    let out = ok(&mote(
        td.path(),
        &["--json", "session", "start", "--as", actor],
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    v["session_id"].as_str().unwrap().to_string()
}

fn mote_in_session(dir: &Path, actor: &str, session: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(mote_bin());
    cmd.args(args)
        .current_dir(dir)
        .env_remove("MOTE_STORE")
        .env_remove("MOTE_SESSION")
        .env("MOTE_ACTOR", actor);
    if let Some(s) = session {
        cmd.env("MOTE_SESSION", s);
    }
    cmd.output().unwrap()
}

#[test]
fn second_session_of_same_actor_cannot_renew_a_session_bound_claim() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "contested", "1");
    let s1 = start_session(&td, "worker");
    let s2 = start_session(&td, "worker");

    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &id],
    ));
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.beads[&id].claim.as_ref().unwrap().session.as_deref(),
        Some(s1.as_str())
    );

    let other = mote_in_session(td.path(), "worker", Some(&s2), &["claim", &id]);
    assert_eq!(other.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&other.stderr);
    assert!(
        stderr.contains(&s1),
        "rejection names the holder session: {stderr}"
    );

    // A sessionless process sharing the name is rejected too, and warned.
    let bare = mote_in_session(td.path(), "worker", None, &["claim", &id]);
    assert_eq!(bare.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bare.stderr).contains("live session(s) share actor worker"));

    // The holding session still renews.
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &id],
    ));

    // Once the holding session ends, the other session may take over.
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["session", "end"],
    ));
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s2),
        &["claim", &id],
    ));
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.beads[&id].claim.as_ref().unwrap().session.as_deref(),
        Some(s2.as_str())
    );
}

#[test]
fn begin_binds_its_claim_to_the_session() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "begin in session", "1");
    let s1 = start_session(&td, "worker");
    let s2 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["begin", &id],
    ));
    let other = mote_in_session(td.path(), "worker", Some(&s2), &["begin", &id]);
    assert_eq!(other.status.code(), Some(2));
}

#[test]
fn sessionless_same_actor_renewal_is_unchanged() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "script", "1");
    ok(&mote(td.path(), &["claim", &id, "--actor", "solo"]));
    let renew = mote(td.path(), &["claim", &id, "--actor", "solo"]);
    ok(&renew);
    assert!(String::from_utf8_lossy(&renew.stderr).is_empty());
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.beads[&id].claim.as_ref().unwrap().session.is_none());
}

#[test]
fn stale_session_id_is_not_attached_to_a_claim() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "stale env", "1");
    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["session", "end"],
    ));
    let out = mote_in_session(td.path(), "worker", Some(&s1), &["claim", &id]);
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a live session"));
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.beads[&id].claim.as_ref().unwrap().session.is_none());
}

#[test]
fn next_claims_by_priority_then_id_and_sets_doing() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let low = new_bead(&td, "low", "3");
    let high = new_bead(&td, "high", "0");
    let mid = new_bead(&td, "mid", "1");
    ok(&mote(
        td.path(),
        &["tag", "add", &mid, "lane-a", "--actor", "alice"],
    ));

    let first = ok(&mote(td.path(), &["next", "--actor", "w1"]));
    assert_eq!(first.trim(), high);
    let json: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "next", "--ttl", "120", "--actor", "w2"],
    )))
    .unwrap();
    assert_eq!(json["id"], mid.as_str());
    assert_eq!(json["status"], "doing");
    assert_eq!(json["claimed_by"], "w2");
    assert!(json["resumed_from"].is_null());

    // Tag filter and exclusions narrow the pool.
    let none = mote(td.path(), &["next", "--tag", "lane-a", "--actor", "w3"]);
    assert_eq!(
        none.status.code(),
        Some(5),
        "mid is taken; no other lane-a work"
    );
    let excluded = mote(td.path(), &["next", "--exclude", &low, "--actor", "w3"]);
    assert_eq!(excluded.status.code(), Some(5));
    assert_eq!(ok(&mote(td.path(), &["next", "--actor", "w3"])).trim(), low);

    let state = reducer::replay_store(&store).unwrap();
    for (id, who) in [(&high, "w1"), (&mid, "w2"), (&low, "w3")] {
        assert_eq!(state.beads[id].status.as_str(), "doing");
        assert_eq!(state.beads[id].claim.as_ref().unwrap().claimed_by, who);
    }
}

#[test]
fn next_on_empty_queue_exits_5_with_json_null() {
    let td = TempDir::new().unwrap();
    init(&td);
    let out = mote(td.path(), &["next", "--actor", "w1"]);
    assert_eq!(out.status.code(), Some(5));
    assert!(out.stdout.is_empty());
    let json = mote(td.path(), &["--json", "next", "--actor", "w1"]);
    assert_eq!(json.status.code(), Some(5));
    assert_eq!(String::from_utf8_lossy(&json.stdout).trim(), "null");
}

#[test]
fn next_resumes_stranded_doing_work() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "abandoned", "1");
    ok(&mote(td.path(), &["begin", &id, "--actor", "alice"]));
    ok(&mote(td.path(), &["release", &id, "--actor", "alice"]));
    let json: serde_json::Value =
        serde_json::from_str(&ok(&mote(td.path(), &["--json", "next", "--actor", "bob"]))).unwrap();
    assert_eq!(json["id"], id.as_str());
    assert_eq!(json["resumed_from"]["reason"], "released");
    assert_eq!(json["resumed_from"]["last_holder"], "alice");
}

#[test]
fn concurrent_next_never_hands_one_bead_to_two_workers() {
    const WORKERS: usize = 4;
    const BEADS: usize = 15;
    let td = TempDir::new().unwrap();
    let store = init(&td);
    for i in 0..BEADS {
        new_bead(&td, &format!("task {i}"), &format!("{}", i % 4));
    }
    let dir = td.path().to_path_buf();
    let handles: Vec<_> = (0..WORKERS)
        .map(|w| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let actor = format!("w{w}");
                let mut won = Vec::new();
                loop {
                    let out = mote(&dir, &["next", "--actor", &actor]);
                    match out.status.code() {
                        Some(0) => {
                            won.push(String::from_utf8(out.stdout).unwrap().trim().to_string())
                        }
                        Some(5) => break,
                        other => panic!(
                            "next failed {other:?}: {}",
                            String::from_utf8_lossy(&out.stderr)
                        ),
                    }
                }
                (actor, won)
            })
        })
        .collect();
    let results: Vec<(String, Vec<String>)> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    let mut all: Vec<&String> = results.iter().flat_map(|(_, w)| w).collect();
    assert_eq!(
        all.len(),
        BEADS,
        "every bead is won exactly once: {results:?}"
    );
    all.sort();
    all.dedup();
    assert_eq!(all.len(), BEADS, "no duplicate winners: {results:?}");

    let state = reducer::replay_store(&store).unwrap();
    for (actor, won) in &results {
        for id in won {
            assert_eq!(&state.beads[id].claim.as_ref().unwrap().claimed_by, actor);
        }
    }
}

#[test]
fn expired_claim_is_not_stranded_while_holder_keeps_a_live_reservation() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "long edit", "1");
    // Claim lease (1s) shorter than the reservation lease, as with defaults.
    ok(&mote(
        td.path(),
        &["reserve", "src/x.rs", "--issue", &id, "--actor", "alice"],
    ));
    ok(&mote(
        td.path(),
        &["claim", &id, "--ttl", "1", "--actor", "alice"],
    ));
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "alice"],
    ));
    std::thread::sleep(std::time::Duration::from_millis(1200));

    assert!(
        ready_json(&td, "bob")
            .iter()
            .all(|b| b["id"] != id.as_str())
    );
    assert_eq!(
        mote(td.path(), &["next", "--actor", "bob"]).status.code(),
        Some(5)
    );
}

#[test]
fn released_claim_is_not_stranded_while_releaser_keeps_a_live_reservation() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "still editing", "1");
    ok(&mote(
        td.path(),
        &["begin", &id, "--paths", "src/y.rs", "--actor", "alice"],
    ));
    ok(&mote(td.path(), &["release", &id, "--actor", "alice"]));
    assert!(
        ready_json(&td, "bob")
            .iter()
            .all(|b| b["id"] != id.as_str())
    );
}

#[test]
fn expired_session_bound_claim_is_not_stranded_while_session_lives() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "session work", "1");
    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &id, "--ttl", "1"],
    ));
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["set", &id, "status=doing"],
    ));
    std::thread::sleep(std::time::Duration::from_millis(1200));
    assert!(
        ready_json(&td, "bob")
            .iter()
            .all(|b| b["id"] != id.as_str())
    );

    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["session", "end"],
    ));
    let entry = ready_json(&td, "bob")
        .into_iter()
        .find(|b| b["id"] == id.as_str())
        .expect("offered once the session ends");
    assert_eq!(entry["stranded"]["reason"], "expired");
}

#[test]
fn never_claimed_doing_bead_is_reported_but_not_handed_out() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "tracked by hand", "1");
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "alice"],
    ));
    assert!(
        ready_json(&td, "bob")
            .iter()
            .all(|b| b["id"] != id.as_str())
    );
    assert_eq!(
        mote(td.path(), &["next", "--actor", "bob"]).status.code(),
        Some(5)
    );
    let board: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "board", "--actor", "bob"],
    )))
    .unwrap();
    assert_eq!(board["stranded"][0]["stranded"]["reason"], "unclaimed");
}

#[test]
fn list_ready_and_show_agree_with_ready_on_stranded_work() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "agree", "1");
    ok(&mote(td.path(), &["begin", &id, "--actor", "alice"]));
    ok(&mote(td.path(), &["release", &id, "--actor", "alice"]));
    let listed: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "ls", "--ready", "--actor", "bob"],
    )))
    .unwrap();
    assert!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"] == id.as_str()),
        "{listed}"
    );
    let shown: serde_json::Value =
        serde_json::from_str(&ok(&mote(td.path(), &["--json", "show", &id]))).unwrap();
    assert_eq!(shown["ready"], true);
}

#[test]
fn session_bound_claims_are_versioned_and_sessionless_claims_keep_v1() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let a = new_bead(&td, "a", "1");
    let b = new_bead(&td, "b", "1");
    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &a],
    ));
    ok(&mote(td.path(), &["claim", &b, "--actor", "solo"]));
    let mut versions = std::collections::BTreeMap::new();
    for name in store.list_op_filenames().unwrap() {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store.ops_dir().join(&name)).unwrap()).unwrap();
        if v["kind"] == "claim" || v.get("ttl_s").is_some() && v.get("to").is_some() {
            versions.insert(
                v["entity"].as_str().unwrap().to_string(),
                (v["v"].clone(), v.get("session").cloned()),
            );
        }
    }
    assert_eq!(versions[&a].0, 2);
    assert_eq!(versions[&a].1, Some(serde_json::Value::String(s1)));
    assert_eq!(versions[&b].0, 1);
    assert_eq!(versions[&b].1, None);
}

#[test]
fn done_rechecks_the_holder_atomically_with_the_close() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "raced", "1");
    let dir = td.path().to_path_buf();
    let id2 = id.clone();
    // Alice passes the early check on an unclaimed bead, then stalls before
    // closing; bob claims in that window.
    let alice = std::thread::spawn(move || {
        Command::new(mote_bin())
            .args(["done", &id2, "--actor", "alice"])
            .env("MOTE_TEST_DELAY_BEFORE_CLOSE_MS", "600")
            .env_remove("MOTE_ACTOR")
            .env_remove("MOTE_STORE")
            .current_dir(&dir)
            .output()
            .unwrap()
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    ok(&mote(td.path(), &["claim", &id, "--actor", "bob"]));
    let out = alice.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("claimed by bob"));
    let state = reducer::replay_store(&store).unwrap();
    assert_ne!(state.beads[&id].status.as_str(), "closed");
}

fn claim_lease(store: &Store, id: &str) -> String {
    let state = reducer::replay_store(store).unwrap();
    state.beads[id]
        .claim
        .as_ref()
        .unwrap()
        .lease_until_ts
        .clone()
}

fn session_lease(store: &Store, sid: &str) -> String {
    let state = reducer::replay_store(store).unwrap();
    state.sessions[sid].lease_until_ts.clone()
}

#[test]
fn session_bound_claim_lives_as_long_as_its_session() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "long task", "1");
    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &id, "--ttl", "60"],
    ));
    assert_eq!(claim_lease(&store, &id), session_lease(&store, &s1));

    // A heartbeat that extends the session extends its claims with it.
    std::thread::sleep(std::time::Duration::from_millis(20));
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["session", "heartbeat", "--force"],
    ));
    let renewed = session_lease(&store, &s1);
    assert_eq!(claim_lease(&store, &id), renewed);

    // A sessionless claim keeps its own TTL.
    let other = new_bead(&td, "script task", "1");
    ok(&mote(
        td.path(),
        &["claim", &other, "--ttl", "60", "--actor", "solo"],
    ));
    assert!(claim_lease(&store, &other) < renewed);
}

#[test]
fn ending_a_session_ends_its_claims_and_strands_doing_work() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "worker leaves", "1");
    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["begin", &id],
    ));
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["session", "end"],
    ));

    let state = reducer::replay_store(&store).unwrap();
    let ended = state.sessions[&s1].ended_ts.clone().unwrap();
    assert_eq!(claim_lease(&store, &id), ended);
    let entry = ready_json(&td, "bob")
        .into_iter()
        .find(|b| b["id"] == id.as_str())
        .expect("the ended session's work is offered again");
    assert_eq!(entry["stranded"]["reason"], "expired");
    assert_eq!(entry["stranded"]["last_holder"], "worker");
}

fn authority_version(store: &Store) -> Option<u32> {
    store.read_format().unwrap().authority.map(|a| a.version)
}

#[test]
fn first_session_bound_claim_raises_the_authority_version_fence() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let a = new_bead(&td, "a", "1");
    let b = new_bead(&td, "b", "1");
    ok(&mote(td.path(), &["claim", &a, "--actor", "solo"]));
    assert_eq!(
        authority_version(&store),
        Some(1),
        "sessionless claims keep v1"
    );

    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &b],
    ));
    assert_eq!(authority_version(&store), Some(2));
    // The store stays fully usable by this binary.
    ok(&mote(td.path(), &["ready", "--actor", "solo"]));
    let status: serde_json::Value =
        serde_json::from_str(&ok(&mote(td.path(), &["--json", "authority", "status"]))).unwrap();
    assert_eq!(status["authority_version"], 2);
}

#[test]
fn other_session_of_same_actor_cannot_release_handoff_or_finish_bound_work() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "bound", "1");
    let s1 = start_session(&td, "worker");
    let s2 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["begin", &id],
    ));

    for args in [
        vec!["release", id.as_str()],
        vec!["handoff", id.as_str(), "--to", "bob"],
        vec!["done", id.as_str()],
        vec!["close", id.as_str()],
    ] {
        let out = mote_in_session(td.path(), "worker", Some(&s2), &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!("worker in session {s1}")),
            "{args:?}: {stderr}"
        );
    }
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.beads[&id].claim.as_ref().unwrap().session.as_deref(),
        Some(s1.as_str())
    );
    assert_eq!(state.beads[&id].status.as_str(), "doing");

    // The holding session can.
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["release", &id],
    ));
}

#[test]
fn close_refuses_another_actors_live_claim_without_force() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "held", "1");
    ok(&mote(td.path(), &["claim", &id, "--actor", "alice"]));
    let out = mote(td.path(), &["close", &id, "--actor", "bob"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("claimed by alice"));
    ok(&mote(
        td.path(),
        &["close", &id, "--force", "--actor", "bob"],
    ));
    let unclaimed = new_bead(&td, "free", "1");
    ok(&mote(td.path(), &["close", &unclaimed, "--actor", "bob"]));
    ok(&mote(td.path(), &["close", &id, "--actor", "alice"]));
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.beads[&id].status.as_str(), "closed");
    assert_eq!(state.beads[&unclaimed].status.as_str(), "closed");
}

#[test]
fn refused_done_leaves_no_completion_note() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "raced note", "1");
    let dir = td.path().to_path_buf();
    let id2 = id.clone();
    let alice = std::thread::spawn(move || {
        Command::new(mote_bin())
            .args(["done", &id2, "--note", "finished", "--actor", "alice"])
            .env("MOTE_TEST_DELAY_BEFORE_CLOSE_MS", "600")
            .env_remove("MOTE_ACTOR")
            .env_remove("MOTE_STORE")
            .current_dir(&dir)
            .output()
            .unwrap()
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    ok(&mote(td.path(), &["claim", &id, "--actor", "bob"]));
    assert_eq!(alice.join().unwrap().status.code(), Some(2));
    let state = reducer::replay_store(&store).unwrap();
    assert!(
        state.beads[&id].notes.iter().all(|n| n.text != "finished"),
        "{:?}",
        state.beads[&id].notes
    );
}

#[test]
fn next_resume_returns_the_callers_own_unfinished_work_first() {
    let td = TempDir::new().unwrap();
    init(&td);
    let mine = new_bead(&td, "mine", "3");
    let fresh = new_bead(&td, "fresh", "0");
    ok(&mote(td.path(), &["begin", &mine, "--actor", "w1"]));

    // Default next never hands back held work, even the caller's own.
    assert_eq!(
        ok(&mote(td.path(), &["next", "--actor", "w1"])).trim(),
        fresh
    );
    ok(&mote(td.path(), &["release", &fresh, "--actor", "w1"]));
    ok(&mote(
        td.path(),
        &["set", &fresh, "status=open", "--actor", "w1"],
    ));

    let json: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "next", "--resume", "--actor", "w1"],
    )))
    .unwrap();
    assert_eq!(
        json["id"],
        mine.as_str(),
        "own work outranks higher-priority new work"
    );
    assert_eq!(json["resumed_from"]["reason"], "own_claim");
    // Other actors are unaffected by --resume.
    let other: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "next", "--resume", "--actor", "w2"],
    )))
    .unwrap();
    assert_eq!(other["id"], fresh.as_str());
}

#[test]
fn next_resume_recovers_an_expired_claim_kept_out_of_ready_by_own_reservation() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "crashed mid-edit", "1");
    ok(&mote(
        td.path(),
        &["reserve", "src/z.rs", "--issue", &id, "--actor", "w1"],
    ));
    ok(&mote(
        td.path(),
        &["claim", &id, "--ttl", "1", "--actor", "w1"],
    ));
    ok(&mote(
        td.path(),
        &["set", &id, "status=doing", "--actor", "w1"],
    ));
    std::thread::sleep(std::time::Duration::from_millis(1200));

    assert_eq!(
        mote(td.path(), &["next", "--actor", "w1"]).status.code(),
        Some(5)
    );
    let json: serde_json::Value = serde_json::from_str(&ok(&mote(
        td.path(),
        &["--json", "next", "--resume", "--actor", "w1"],
    )))
    .unwrap();
    assert_eq!(json["id"], id.as_str());
    assert_eq!(json["resumed_from"]["reason"], "own_expired_claim");
}

#[test]
fn next_resume_does_not_take_work_bound_to_another_live_session() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "s1 work", "1");
    let s1 = start_session(&td, "worker");
    let s2 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["begin", &id],
    ));
    let out = mote_in_session(td.path(), "worker", Some(&s2), &["next", "--resume"]);
    assert_eq!(out.status.code(), Some(5));
}

#[test]
fn release_force_lets_a_sessionless_shell_give_up_session_bound_work() {
    let td = TempDir::new().unwrap();
    let store = init(&td);
    let id = new_bead(&td, "bound", "1");
    let s1 = start_session(&td, "worker");
    ok(&mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["begin", &id],
    ));
    let refused = mote_in_session(td.path(), "worker", None, &["release", &id]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
    let forced = mote_in_session(td.path(), "worker", None, &["release", &id, "--force"]);
    ok(&forced);
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.beads[&id].claim.is_none());
}

#[test]
fn claim_notes_when_the_session_lease_outlives_the_requested_ttl() {
    let td = TempDir::new().unwrap();
    init(&td);
    let id = new_bead(&td, "ttl", "1");
    let s1 = start_session(&td, "worker");
    let out = mote_in_session(
        td.path(),
        "worker",
        Some(&s1),
        &["claim", &id, "--ttl", "60"],
    );
    ok(&out);
    assert!(String::from_utf8_lossy(&out.stderr).contains("lives until the session lease"));
}
