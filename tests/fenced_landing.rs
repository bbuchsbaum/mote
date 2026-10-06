use mote::{
    Store, authority,
    candidate::AuthorizationStatus,
    ids,
    op::{CandidateRevokeOp, Op},
    publish, reducer,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn command(root: &Path, actor: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mote"));
    cmd.current_dir(root)
        .args([
            "--store",
            root.to_str().unwrap(),
            "--actor",
            actor,
            "--json",
        ])
        .args(args);
    cmd
}
fn run(root: &Path, actor: &str, args: &[&str]) -> Value {
    let out = command(root, actor, args).output().unwrap();
    assert!(
        out.status.success(),
        "mote {args:?}: {} {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
struct Fixture {
    root: TempDir,
    candidate: String,
    before: String,
    after: String,
    phase: String,
    authorization: String,
}
impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let p = root.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.name", "Test"]);
        git(p, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(p.join("work.txt"), "base\n").unwrap();
        git(p, &["add", "work.txt"]);
        git(p, &["commit", "-qm", "base"]);
        let before = git(p, &["rev-parse", "HEAD"]);
        let store = Store::init(p).unwrap();
        let issue = ids::new_bead_id();
        publish::publish_op(
            &store,
            &mote::op::make_create(
                "author".into(),
                issue.clone(),
                mote::op::ScalarSet {
                    title: Some("fenced landing".into()),
                    ..Default::default()
                },
                jiff::Timestamp::now(),
            ),
        )
        .unwrap();
        git(p, &["switch", "-qc", "candidate"]);
        std::fs::write(p.join("work.txt"), "candidate\n").unwrap();
        git(p, &["commit", "-qam", "candidate"]);
        let after = git(p, &["rev-parse", "HEAD"]);
        let proposal = run(
            p,
            "proposer",
            &[
                "candidate",
                "propose",
                "--issue",
                &issue,
                "--base",
                &before,
                "--path",
                "work.txt",
                "--authorizer",
                "author",
                "--reviewer",
                "reviewer",
                "--idempotency-key",
                "proposal",
            ],
        );
        let candidate = proposal["candidate_id"].as_str().unwrap().to_string();
        run(
            p,
            "reviewer",
            &[
                "candidate",
                "review",
                &candidate,
                "approve",
                "--idempotency-key",
                "review",
            ],
        );
        let auth = run(
            p,
            "author",
            &[
                "candidate",
                "authorize",
                &candidate,
                "--grantee",
                "lander",
                "--idempotency-key",
                "auth",
            ],
        );
        run(
            p,
            "author",
            &[
                "candidate",
                "evidence",
                "target-scope",
                &candidate,
                "--target",
                "main",
                "--idempotency-key",
                "scope",
            ],
        );
        Self {
            root,
            candidate,
            before,
            after,
            phase: auth["phase"]["op_id"].as_str().unwrap().into(),
            authorization: auth["authorization"]["op_id"].as_str().unwrap().into(),
        }
    }
    fn store(&self) -> Store {
        Store::open(&self.root.path().join(".mote")).unwrap()
    }
    fn land(&self, actor: &str) -> Command {
        command(
            self.root.path(),
            actor,
            &[
                "candidate",
                "land",
                &self.candidate,
                "--target",
                "main",
                "--before",
                &self.before,
                "--expect-phase",
                &self.phase,
                "--expect-authorization",
                &self.authorization,
                "--idempotency-key",
                "landing",
            ],
        )
    }
    fn revoke(&self) -> Command {
        command(
            self.root.path(),
            "author",
            &[
                "candidate",
                "revoke",
                &self.candidate,
                "--expect",
                &self.authorization,
                "--idempotency-key",
                "revoke",
            ],
        )
    }
    fn tip(&self) -> String {
        git(self.root.path(), &["rev-parse", "main"])
    }
}
fn payload(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}
fn wait_signal(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing checkpoint signal");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn exact_landing_and_byte_identical_retry() {
    let f = Fixture::new();
    let out = f.land("lander").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let receipt = payload(&out);
    assert_eq!(receipt["git_updated"], true);
    assert_eq!(receipt["old_oid"], f.before);
    assert_eq!(receipt["new_oid"], f.after);
    assert_eq!(f.tip(), f.after);
    let files = f.store().list_op_filenames().unwrap();
    let retry = f.land("lander").output().unwrap();
    assert!(retry.status.success(), "{retry:?}");
    assert_eq!(payload(&retry)["retry"], true);
    assert_eq!(files, f.store().list_op_filenames().unwrap());
    assert_eq!(
        reducer::replay_store(&f.store()).unwrap().candidates[&f.candidate]
            .authorization
            .as_ref()
            .unwrap()
            .status,
        AuthorizationStatus::Consumed
    );
}

#[test]
fn revoke_before_authority_boundary_and_non_grantee_leave_git_unchanged() {
    let f = Fixture::new();
    let view = run(
        f.root.path(),
        "intruder",
        &["candidate", "show", &f.candidate],
    );
    assert_eq!(view["landability_actor"], "intruder");
    assert_eq!(view["landability"]["landable"], false);
    assert!(
        view["landability"]["reason_codes"]
            .as_array()
            .unwrap()
            .contains(&json!("actor_not_grantee"))
    );
    assert!(!f.land("intruder").output().unwrap().status.success());
    assert_eq!(f.tip(), f.before);
    assert!(f.revoke().output().unwrap().status.success());
    assert!(!f.land("lander").output().unwrap().status.success());
    assert_eq!(f.tip(), f.before);
}

#[test]
fn revocation_between_final_read_and_ref_update_is_fenced() {
    let f = Fixture::new();
    let signal = f.root.path().join("paused");
    let land = f
        .land("lander")
        .env("MOTE_TEST_AUTHORITY_PAUSE", "landing-before-update")
        .env("MOTE_TEST_AUTHORITY_SIGNAL", &signal)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_signal(&signal);
    let mut revoke = f
        .revoke()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        revoke.try_wait().unwrap().is_none(),
        "revoker crossed held fence"
    );
    std::fs::remove_file(signal).unwrap();
    let landed = land.wait_with_output().unwrap();
    assert!(landed.status.success(), "{landed:?}");
    assert!(!revoke.wait_with_output().unwrap().status.success());
    assert_eq!(f.tip(), f.after);
}

#[test]
fn late_earlier_stamped_revoke_cannot_rewrite_committed_authority() {
    let f = Fixture::new();
    let earlier = ids::format_rfc3339(jiff::Timestamp::now());
    assert!(f.land("lander").output().unwrap().status.success());
    let store = f.store();
    let revoke = publish::publish_op(
        &store,
        &Op::CandidateRevoke(CandidateRevokeOp {
            v: 1,
            op: String::new(),
            ts: earlier,
            actor: "author".into(),
            candidate_id: f.candidate.clone(),
            expect_authorization: f.authorization.clone(),
            reason: None,
            idempotency_key: "late".into(),
        }),
    )
    .unwrap();
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.was_accepted(revoke.as_str()));
    assert_eq!(
        state.candidates[&f.candidate]
            .authorization
            .as_ref()
            .unwrap()
            .status,
        AuthorizationStatus::Consumed
    );
    assert_eq!(f.tip(), f.after);
}

#[test]
fn restart_at_every_durable_landing_boundary() {
    for step in [
        "landing-prepared",
        "landing-before-update",
        "landing-after-update",
        "landing-updated",
        "publication-prepared",
        "publication-linked",
        "publication-admitted",
        "landing-evidence",
        "landing-confirmed-op",
        "landing-archived",
    ] {
        let f = Fixture::new();
        let interrupted = f
            .land("lander")
            .env("MOTE_TEST_AUTHORITY_FAIL", step)
            .output()
            .unwrap();
        assert!(!interrupted.status.success(), "{step}: {interrupted:?}");
        let receipt = payload(&interrupted);
        assert_eq!(receipt["outcome"], "recovery_required", "{step}");
        assert!(
            Path::new(receipt["journal"].as_str().unwrap()).exists(),
            "{step}"
        );
        let updated = f.tip() == f.after;
        if updated {
            assert_eq!(receipt["git_updated"], true, "{step}: {receipt}");
        }
        assert!(
            !f.revoke().output().unwrap().status.success(),
            "{step}: interrupted landing admitted revoke"
        );
        let recovered = f.land("lander").output().unwrap();
        assert!(recovered.status.success(), "{step}: {recovered:?}");
        assert_eq!(f.tip(), f.after, "{step}");
        assert!(
            !authority::directory(&f.store())
                .join("landing-active.json")
                .exists()
        );
    }
}

#[test]
fn changed_ref_after_git_update_reports_truth_and_does_not_reset() {
    let f = Fixture::new();
    let interrupted = f
        .land("lander")
        .env("MOTE_TEST_AUTHORITY_FAIL", "landing-after-update")
        .output()
        .unwrap();
    assert_eq!(payload(&interrupted)["git_updated"], true);
    git(
        f.root.path(),
        &["update-ref", "refs/heads/main", &f.before, &f.after],
    );
    let recovered = f.land("lander").output().unwrap();
    assert!(!recovered.status.success());
    let receipt = payload(&recovered);
    assert_eq!(receipt["git_updated"], true);
    assert_eq!(receipt["current_oid"], f.before);
    assert_eq!(f.tip(), f.before);
}

#[test]
#[cfg(debug_assertions)]
fn killed_process_releases_lock_but_retains_authority_for_recovery() {
    for checkpoint in ["landing-before-update", "landing-after-update"] {
        let f = Fixture::new();
        let signal = f.root.path().join("kill-checkpoint");
        let mut child = f
            .land("lander")
            .env("MOTE_TEST_AUTHORITY_PAUSE", checkpoint)
            .env("MOTE_TEST_AUTHORITY_SIGNAL", &signal)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_signal(&signal);
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!f.revoke().output().unwrap().status.success());
        let retry = f.land("lander").output().unwrap();
        assert!(retry.status.success(), "{checkpoint}: {retry:?}");
        assert_eq!(f.tip(), f.after);
    }
}

#[test]
#[cfg(debug_assertions)]
fn rejected_confirmation_never_claims_git_was_unchanged() {
    let f = Fixture::new();
    let signal = f.root.path().join("confirmation-checkpoint");
    let child = f
        .land("lander")
        .env("MOTE_TEST_AUTHORITY_PAUSE", "landing-after-update")
        .env("MOTE_TEST_AUTHORITY_SIGNAL", &signal)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_signal(&signal);
    let store = f.store();
    let names = store.list_op_filenames().unwrap();
    let damaged = store.ops_dir().join(&names[0]);
    let original = std::fs::read(&damaged).unwrap();
    std::fs::write(&damaged, b"{}").unwrap();
    std::fs::remove_file(signal).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let receipt = payload(&result);
    assert_eq!(receipt["git_updated"], true);
    assert_eq!(receipt["old_oid"], f.before);
    assert_eq!(receipt["new_oid"], f.after);
    assert_eq!(receipt["current_oid"], f.after);
    assert_eq!(receipt["outcome"], "recovery_required");
    assert_eq!(f.tip(), f.after);
    std::fs::write(damaged, original).unwrap();
    assert!(f.land("lander").output().unwrap().status.success());
}

#[test]
#[cfg(debug_assertions)]
fn moved_preimage_before_any_git_attempt_aborts_and_unblocks_writers() {
    let f = Fixture::new();
    let signal = f.root.path().join("preimage-checkpoint");
    let child = f
        .land("lander")
        .env("MOTE_TEST_AUTHORITY_PAUSE", "landing-prepared")
        .env("MOTE_TEST_AUTHORITY_SIGNAL", &signal)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_signal(&signal);
    git(
        f.root.path(),
        &["update-ref", "refs/heads/main", &f.after, &f.before],
    );
    std::fs::remove_file(signal).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert_eq!(payload(&out)["git_updated"], false);
    assert_eq!(payload(&out)["outcome"], "aborted");
    assert_eq!(f.tip(), f.after);
    assert!(
        !authority::directory(&f.store())
            .join("landing-active.json")
            .exists()
    );
    assert!(f.revoke().output().unwrap().status.success());
}
