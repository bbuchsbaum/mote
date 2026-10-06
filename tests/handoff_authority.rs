use jiff::Timestamp;
use mote::{
    Store,
    handoff::{self, HandoffOp},
    ids,
    op::{self, Op, ScalarSet},
    publish, reducer,
};
use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
    time::Duration,
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    store: Store,
    issue: String,
    token: String,
}
impl Fixture {
    fn new(ttl: u32) -> Self {
        let root = TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        let issue = ids::new_bead_id();
        publish::publish_op(
            &store,
            &op::make_create(
                "alice".into(),
                issue.clone(),
                ScalarSet {
                    title: Some("handoff".into()),
                    ..Default::default()
                },
                Timestamp::now(),
            ),
        )
        .unwrap();
        let token = publish::publish_op(
            &store,
            &op::make_claim(
                "alice".into(),
                issue.clone(),
                "alice".into(),
                ttl,
                None,
                Timestamp::now(),
            ),
        )
        .unwrap()
        .into_string();
        Self {
            root,
            store,
            issue,
            token,
        }
    }
    fn request(&self) -> HandoffOp {
        HandoffOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "alice".into(),
            entity: self.issue.clone(),
            to: "carol".into(),
            expect_holder: "alice".into(),
            expect_claim: self.token.clone(),
            ttl_s: 300,
            note: "exact handoff".into(),
            release: false,
            idempotency_key: "transfer".into(),
        }
    }
    fn command(&self, key: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mote"));
        command.current_dir(self.root.path()).args([
            "--actor",
            "alice",
            "--json",
            "handoff",
            &self.issue,
            "--to",
            "carol",
            "--expect-holder",
            "alice",
            "--expect-claim",
            &self.token,
            "--idempotency-key",
            key,
        ]);
        command
    }
    fn holder(&self) -> String {
        reducer::replay_store(&self.store).unwrap().beads[&self.issue]
            .claim
            .as_ref()
            .unwrap()
            .claimed_by
            .clone()
    }
}
fn payload(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| panic!("{out:?}"))
}

#[test]
fn stale_precheck_cannot_transfer_replacement_claim_or_write_note() {
    let f = Fixture::new(0);
    let replacement = publish::publish_op(
        &f.store,
        &op::make_claim(
            "bob".into(),
            f.issue.clone(),
            "bob".into(),
            300,
            None,
            Timestamp::now(),
        ),
    )
    .unwrap();
    let (code, receipt) = handoff::execute(&f.store, f.request(), true).unwrap();
    assert_eq!(code, 2);
    assert_eq!(receipt["outcome"], "conflict");
    assert_eq!(receipt["current_claim"]["token"], replacement.as_str());
    assert_eq!(f.holder(), "bob");
    assert!(
        reducer::replay_store(&f.store).unwrap().beads[&f.issue]
            .notes
            .is_empty()
    );
    let count = f.store.list_op_filenames().unwrap();
    assert_eq!(handoff::execute(&f.store, f.request(), true).unwrap().0, 2);
    assert_eq!(count, f.store.list_op_filenames().unwrap());
}

#[test]
fn nonholder_and_raw_claim_cas_do_not_bypass_authority() {
    let f = Fixture::new(300);
    let mut request = f.request();
    request.actor = "mallory".into();
    let name = publish::publish_op(&f.store, &Op::Handoff(request)).unwrap();
    let state = reducer::replay_store(&f.store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    let bypass = publish::publish_op(
        &f.store,
        &op::make_claim(
            "mallory".into(),
            f.issue.clone(),
            "carol".into(),
            300,
            Some(f.token.clone()),
            Timestamp::now(),
        ),
    )
    .unwrap();
    assert!(
        !reducer::replay_store(&f.store)
            .unwrap()
            .was_accepted(bypass.as_str())
    );
    assert_eq!(f.holder(), "alice");
}

#[test]
fn expired_claim_and_changed_retry_payload_are_explicit() {
    let f = Fixture::new(0);
    let (code, result) = handoff::execute(&f.store, f.request(), true).unwrap();
    assert_eq!(code, 2);
    assert_eq!(result["outcome"], "expired");
    let mut changed = f.request();
    changed.to = "bob".into();
    assert!(
        handoff::execute(&f.store, changed, true)
            .unwrap_err()
            .to_string()
            .contains("idempotency conflict")
    );
}

#[test]
fn competing_handoffs_have_one_winner_and_retry_does_not_transfer_again() {
    let f = Fixture::new(300);
    let first = f
        .command("first")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = f
        .command("second")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let a = first.wait_with_output().unwrap();
    let b = second.wait_with_output().unwrap();
    assert_ne!(a.status.success(), b.status.success());
    assert_eq!(f.holder(), "carol");
    let winner = if a.status.success() {
        "first"
    } else {
        "second"
    };
    let names = f.store.list_op_filenames().unwrap();
    let retry = f.command(winner).output().unwrap();
    assert!(retry.status.success());
    assert_eq!(payload(&retry)["retry"], true);
    assert_eq!(names, f.store.list_op_filenames().unwrap());
    assert_eq!(
        reducer::replay_store(&f.store).unwrap().beads[&f.issue]
            .notes
            .len(),
        1
    );
}

#[test]
fn crash_at_each_publication_step_reuses_exact_bytes() {
    for step in [
        "publication-prepared",
        "publication-linked",
        "publication-admitted",
    ] {
        let f = Fixture::new(300);
        let interrupted = f
            .command("restart")
            .env("MOTE_TEST_AUTHORITY_FAIL", step)
            .output()
            .unwrap();
        assert!(!interrupted.status.success());
        let journal = mote::authority::directory(&f.store).join("publication.json");
        let record: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        let original = record["bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u8)
            .collect::<Vec<_>>();
        let retry = f.command("restart").output().unwrap();
        assert!(retry.status.success(), "{step}: {retry:?}");
        let path = f
            .store
            .ops_dir()
            .join(format!("{}.json", record["name"].as_str().unwrap()));
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(f.holder(), "carol");
        assert_eq!(
            reducer::replay_store(&f.store).unwrap().beads[&f.issue]
                .notes
                .len(),
            1
        );
    }
}

fn reserve(f: &Fixture, ttl: u32) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(f.root.path())
        .args([
            "--actor",
            "alice",
            "reserve",
            "--issue",
            &f.issue,
            "src",
            "--ttl",
            &ttl.to_string(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn adopt(f: &Fixture, reservation: &str, token: &str) -> bool {
    let name = publish::publish_op(
        &f.store,
        &Op::ReserveAdopt(op::ReserveAdoptOp {
            v: 1,
            op: String::new(),
            ts: ids::format_rfc3339(Timestamp::now()),
            actor: "carol".into(),
            reservation_id: reservation.into(),
            entity: f.issue.clone(),
            expect_reservation: token.into(),
            ttl_s: 300,
        }),
    )
    .unwrap();
    reducer::replay_store(&f.store)
        .unwrap()
        .was_accepted(name.as_str())
}

#[test]
fn handoff_leaves_carrier_orphaned_and_adoption_requires_live_ttl() {
    let f = Fixture::new(300);
    let rv = reserve(&f, 1);
    let token = reducer::replay_store(&f.store).unwrap().reservations[&rv]
        .clock
        .clone();
    let (code, result) = handoff::execute(&f.store, f.request(), true).unwrap();
    assert_eq!(code, 0);
    assert_eq!(result["reservations_transferred"], false);
    assert_eq!(result["reservations"][0]["disposition"], "orphaned");
    std::thread::sleep(Duration::from_millis(1100));
    assert!(!adopt(&f, &rv, &token));
    let state = reducer::replay_store(&f.store).unwrap();
    assert_eq!(state.reservations[&rv].actor, "alice");
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&rv],
            &ids::format_rfc3339(Timestamp::now())
        ),
        mote::state::LeaseDisposition::Expired
    );
}

#[test]
fn reservation_release_is_atomic_with_successful_handoff() {
    let f = Fixture::new(300);
    let rv = reserve(&f, 300);
    let mut request = f.request();
    request.release = true;
    let (_, result) = handoff::execute(&f.store, request, true).unwrap();
    assert_eq!(result["reservations"][0]["disposition"], "closed");
    let state = reducer::replay_store(&f.store).unwrap();
    assert_eq!(
        state.reservations[&rv].clock,
        state.beads[&f.issue].claim.as_ref().unwrap().claim_clock
    );
}

#[test]
fn no_precondition_pair_is_silently_completed() {
    let f = Fixture::new(300);
    let out = Command::new(env!("CARGO_BIN_EXE_mote"))
        .current_dir(f.root.path())
        .args([
            "--actor",
            "alice",
            "handoff",
            &f.issue,
            "--to",
            "carol",
            "--expect-holder",
            "alice",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(f.holder(), "alice");
    assert!(!Path::new(&mote::authority::directory(&f.store)).exists());
}

#[test]
fn competing_carrier_adoptions_have_one_winner_with_exact_token() {
    let f = Fixture::new(300);
    let rv = reserve(&f, 300);
    let token = reducer::replay_store(&f.store).unwrap().reservations[&rv]
        .clock
        .clone();
    handoff::execute(&f.store, f.request(), true).unwrap();
    let make = || {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mote"));
        cmd.current_dir(f.root.path()).args([
            "--actor",
            "carol",
            "--json",
            "adopt",
            &rv,
            "--issue",
            &f.issue,
            "--expect-reservation",
            &token,
        ]);
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        cmd
    };
    let a = make().spawn().unwrap();
    let b = make().spawn().unwrap();
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert_ne!(a.status.success(), b.status.success(), "{a:?} {b:?}");
    let state = reducer::replay_store(&f.store).unwrap();
    assert_eq!(state.reservations[&rv].adoptions.len(), 1);
    assert_eq!(state.reservations[&rv].actor, "carol");
    assert_eq!(
        state.reservation_disposition(
            &state.reservations[&rv],
            &ids::format_rfc3339(Timestamp::now())
        ),
        mote::state::LeaseDisposition::Active
    );
}

#[test]
#[cfg(debug_assertions)]
fn restart_after_new_lease_expires_reports_historical_transfer_only() {
    let f = Fixture::new(300);
    let mut format = f.store.read_format().unwrap();
    format.default_ttl_s.claim = 1;
    std::fs::write(f.store.format_path(), serde_json::to_vec(&format).unwrap()).unwrap();
    let interrupted = f
        .command("short-lease")
        .env("MOTE_TEST_AUTHORITY_FAIL", "publication-prepared")
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    assert_eq!(payload(&interrupted)["outcome"], "recovery_required");
    std::thread::sleep(Duration::from_millis(1100));
    let retry = f.command("short-lease").output().unwrap();
    assert!(retry.status.success(), "{retry:?}");
    let receipt = payload(&retry);
    assert_eq!(receipt["accepted"], true);
    assert_eq!(receipt["outcome"], "applied_no_longer_current");
    assert_eq!(receipt["claim_current"], false);
}

#[test]
fn public_claim_token_does_not_authorize_release_or_release_a_replacement() {
    let f = Fixture::new(300);
    let foreign = publish::publish_op(
        &f.store,
        &op::make_release(
            "mallory".into(),
            f.issue.clone(),
            Some(f.token.clone()),
            Timestamp::now(),
        ),
    )
    .unwrap();
    assert!(
        !reducer::replay_store(&f.store)
            .unwrap()
            .was_accepted(foreign.as_str())
    );
    let replacement = publish::publish_op(
        &f.store,
        &op::make_claim(
            "alice".into(),
            f.issue.clone(),
            "alice".into(),
            300,
            Some(f.token.clone()),
            Timestamp::now(),
        ),
    )
    .unwrap();
    let stale = publish::publish_op(
        &f.store,
        &op::make_release(
            "alice".into(),
            f.issue.clone(),
            Some(f.token.clone()),
            Timestamp::now(),
        ),
    )
    .unwrap();
    let state = reducer::replay_store(&f.store).unwrap();
    assert!(!state.was_accepted(stale.as_str()));
    assert_eq!(
        state.beads[&f.issue].claim.as_ref().unwrap().claim_clock,
        replacement.as_str()
    );
    let release = publish::publish_op(
        &f.store,
        &op::make_release(
            "alice".into(),
            f.issue.clone(),
            Some(replacement.into_string()),
            Timestamp::now(),
        ),
    )
    .unwrap();
    assert!(
        reducer::replay_store(&f.store)
            .unwrap()
            .was_accepted(release.as_str())
    );
}
