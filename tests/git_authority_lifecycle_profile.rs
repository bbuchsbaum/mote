//! Contract-derived lifecycle witnesses and real-Git multi-client recovery.
use mote::git_authority::{
    client::{Client, Submission, SubmissionStatus},
    planner::{AuthorityState, plan},
    protocol::{Manifest, OutcomeStatus, Request, Transaction, decode, encode},
    transport::GitTransport,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::Command,
    sync::{Arc, Barrier},
    thread,
};
use tempfile::{TempDir, tempdir};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/git_authority/lifecycle.json")).unwrap()
}
fn request(id: &str, actor: &str, session: &str, action: Value) -> Request {
    serde_json::from_value(
        json!({"version":1,"request_id":id,"actor":actor,"session":session,
        "timestamp":"2026-10-20T00:00:00Z","command":{"kind":"coordination","action":action}}),
    )
    .unwrap()
}
fn apply(state: &mut AuthorityState, request: Request) {
    let p = plan(state, &request).unwrap();
    assert_eq!(p.outcome.status, OutcomeStatus::Accepted, "{:?}", p.outcome);
    *state = p.next_state;
}
fn seeded(scenario: usize) -> AuthorityState {
    let mut state = AuthorityState::default();
    for seed in fixture()["scenarios"][scenario]["seed"].as_array().unwrap() {
        apply(&mut state, serde_json::from_value(seed.clone()).unwrap());
    }
    state
}
fn rejects(state: &AuthorityState, r: Request, code: &str, subject: &str) {
    let p = plan(state, &r).unwrap();
    assert!(p.effects.is_empty());
    assert_eq!(p.outcome.status, OutcomeStatus::Rejected);
    assert_eq!(p.outcome.result, json!({"code":code,"subject":subject}));
    assert_eq!(p.next_state.coordination, state.coordination);
    assert_eq!(p.next_state.sequence, state.sequence + 1);
}
#[test]
fn independent_lifecycle_vectors_pin_effects_results_and_tokens() {
    for (i, scenario) in fixture()["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let mut state = seeded(i);
        for v in scenario["vectors"].as_array().unwrap() {
            let r: Request = decode(&serde_json::to_vec(&v["request"]).unwrap()).unwrap();
            let p = plan(&state, &r).unwrap();
            assert_eq!(
                serde_json::to_value(&p.effects).unwrap(),
                v["effects"],
                "{}",
                v["name"]
            );
            assert_eq!(
                serde_json::to_value(&p.outcome).unwrap(),
                v["outcome"],
                "{}",
                v["name"]
            );
            let tx = Transaction::prepare(&state, &"1".repeat(40), r).unwrap();
            assert_eq!(tx.validate(&state, &tx.parent).unwrap(), p.next_state);
            state = p.next_state;
            assert_eq!(state.sequence, v["sequence"].as_u64().unwrap());
        }
        let view = state.coordination.view().unwrap();
        let task = &view["tasks"]["bd-one"];
        match i {
            0 => {
                assert_eq!(task["status"], "doing");
                assert_eq!(
                    task["clock"]["status"],
                    "gaop-00000000000000000004-000001-eff-begin-pathless-000001"
                );
            }
            1 => {
                assert_eq!(
                    task["clock"]["status"],
                    "gaop-00000000000000000004-000002-eff-begin-work-000002"
                );
                assert!(view["claims"].get("bd-one").is_none());
                assert_eq!(view["reservations"]["rv-one"]["status"], "closed");
                assert_eq!(
                    view["reservations"]["rv-one"]["token"],
                    "eff-handoff-work-000002"
                );
                assert_eq!(view["reservations"]["rv-one"]["paths"], json!(["src/a.rs"]));
                assert_eq!(view["sessions"]["sess-b"]["status"], "ended");
                assert_eq!(view["sessions"]["sess-b"]["token"], "eff-enroll-b-000000");
                assert_eq!(view["sessions"]["sess-a"]["status"], "active");
            }
            2 => {
                assert_eq!(task["status"], "open");
                assert_eq!(
                    task["clock"]["status"],
                    "gaop-00000000000000000003-000000-eff-create-000000"
                );
                assert_eq!(view["sessions"]["sess-a"]["status"], "revoked");
                assert_eq!(view["sessions"]["sess-b"]["status"], "active");
                assert_eq!(view["sessions"]["sess-b"]["token"], "eff-enroll-b-000000");
                assert_eq!(view["claims"]["bd-one"]["token"], "eff-recover-a-000000");
                assert_eq!(
                    view["reservations"]["rv-one"]["token"],
                    "eff-recover-a-000001"
                );
            }
            _ => unreachable!(),
        }
    }
}
#[test]
fn lifecycle_syntax_and_profile_are_strict() {
    let good = fixture()["scenarios"][0]["vectors"][0]["request"].clone();
    for field in ["reservation", "note"] {
        let mut missing = good.clone();
        missing["command"]["action"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(decode::<Request>(&serde_json::to_vec(&missing).unwrap()).is_err());
    }
    for (field, value) in [
        ("expect_status", json!(null)),
        ("expect_status", json!("")),
        ("paths", json!(["../src"])),
        ("surprise", json!(true)),
    ] {
        let mut bad = good.clone();
        bad["command"]["action"][field] = value;
        assert!(
            decode::<Request>(&serde_json::to_vec(&bad).unwrap())
                .and_then(|r| r.validate())
                .is_err()
        );
    }
    let r: Request = serde_json::from_value(good).unwrap();
    assert!(
        Manifest::coordination("st-test".into())
            .allows(&r.command)
            .is_err()
    );
    assert!(
        Manifest::lifecycle("st-test".into())
            .allows(&r.command)
            .is_ok()
    );
    let recover = &fixture()["scenarios"][2]["vectors"][0]["request"];
    for (field, value) in [
        ("reason", json!(" \n")),
        ("recipient_session", json!(null)),
        ("disposition", json!("force")),
    ] {
        let mut bad = recover.clone();
        bad["command"]["action"][field] = value;
        assert!(
            decode::<Request>(&serde_json::to_vec(&bad).unwrap())
                .and_then(|r| r.validate())
                .is_err()
        );
    }
}
#[test]
fn recovery_checks_complete_maps_in_sorted_union_order_and_is_atomic() {
    let state = seeded(2);
    let mut r: Request =
        serde_json::from_value(fixture()["scenarios"][2]["vectors"][0]["request"].clone()).unwrap();
    let mut value = serde_json::to_value(&r).unwrap();
    value["command"]["action"]["claims"] = json!({"bd-aaa":"bogus"});
    value["command"]["action"]["reservations"] = json!({});
    r = serde_json::from_value(value.clone()).unwrap();
    rejects(&state, r, "claim_mismatch", "bd-aaa");
    value["command"]["action"]["expect_session"] = json!("wrong");
    value["command"]["action"]["recipient_session"] = json!("missing");
    rejects(
        &state,
        serde_json::from_value(value.clone()).unwrap(),
        "enrollment_mismatch",
        "sess-a",
    );
    value["command"]["action"]["expect_session"] = json!("eff-enroll-a-000000");
    rejects(
        &state,
        serde_json::from_value(value).unwrap(),
        "session_unavailable",
        "missing",
    );
}
#[test]
fn next_orders_priority_then_id_filters_and_never_resumes_a_claim() {
    let mut state = seeded(0);
    for (id, priority) in [("bd-z", 0), ("bd-a", 0), ("bd-b", 1)] {
        apply(
            &mut state,
            request(
                &format!("create-{id}"),
                "alice",
                "sess-a",
                json!({"kind":"task.create","id":id,"title":id,"body":"","priority":priority,"assignee":"someone-else"}),
            ),
        );
    }
    let next = |id: &str, exclude: Value| {
        request(
            id,
            "alice",
            "sess-a",
            json!({"kind":"next","tags":[],"exclude":exclude,"reservation":null,"paths":[],"note":null}),
        )
    };
    let p = plan(&state, &next("first", json!([]))).unwrap();
    assert_eq!(p.outcome.result["task"], "bd-a");
    state = p.next_state;
    let p = plan(&state, &next("second", json!([]))).unwrap();
    assert_eq!(p.outcome.result["task"], "bd-z");
    state = p.next_state;
    let p = plan(&state, &next("third", json!(["bd-b"]))).unwrap();
    assert_eq!(p.outcome.result["task"], "bd-one");
    state = p.next_state;
    rejects(
        &state,
        next("none", json!(["bd-b"])),
        "no_ready_task",
        "next",
    );
}

#[test]
fn done_guards_other_holders_and_handoff_preserves_their_reservations() {
    let mut state = seeded(0);
    let begin: Request =
        serde_json::from_value(fixture()["scenarios"][1]["vectors"][0]["request"].clone()).unwrap();
    apply(&mut state, begin);
    apply(
        &mut state,
        request(
            "other-rv",
            "bob",
            "sess-b",
            json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-other","paths":["docs/a.md"]}),
        ),
    );
    let clock = "gaop-00000000000000000004-000002-eff-begin-work-000002";
    let done = request(
        "done",
        "alice",
        "sess-a",
        json!({"kind":"done","id":"bd-one","expect_status":clock,"expect_claim":"eff-begin-work-000001","reservations":{"rv-one":"eff-begin-work-000000"},"note":"finished"}),
    );
    rejects(&state, done, "held_work", "bd-one");
    let mut handoff = fixture()["scenarios"][1]["vectors"][1]["request"].clone();
    handoff["command"]["action"]["disposition"] = json!("release");
    let p = plan(&state, &serde_json::from_value(handoff).unwrap()).unwrap();
    assert_eq!(p.outcome.status, OutcomeStatus::Accepted);
    assert_eq!(p.outcome.result["reservations"][0]["token"], Value::Null);
    assert_eq!(
        p.outcome.result["unaffected_other_session_reservations"],
        json!([{"id":"rv-other","task":"bd-one","actor":"bob","session":"sess-b","token":"eff-other-rv-000000","paths":["docs/a.md"]}])
    );
    let view = p.next_state.coordination.view().unwrap();
    assert_eq!(view["reservations"]["rv-other"]["status"], "open");
    assert_eq!(view["reservations"]["rv-one"]["status"], "closed");
    assert_eq!(view["claims"]["bd-one"]["session"], "sess-b");
    assert_eq!(view["claims"]["bd-one"]["token"], "eff-handoff-work-000001");
    assert_eq!(view["tasks"]["bd-one"]["clock"]["status"], clock);
}

#[test]
fn release_recovery_retains_audit_tokens_and_stale_maps_reject_every_effect() {
    let state = seeded(2);
    let mut value = fixture()["scenarios"][2]["vectors"][0]["request"].clone();
    value["command"]["action"]["disposition"] = json!("release");
    value["command"]["action"]["recipient_session"] = Value::Null;
    let r: Request = serde_json::from_value(value.clone()).unwrap();
    let p = plan(&state, &r).unwrap();
    assert_eq!(p.outcome.status, OutcomeStatus::Accepted);
    for e in serde_json::to_value(&p.effects)
        .unwrap()
        .as_array()
        .unwrap()
    {
        for field in ["new_actor", "new_session", "new_token"] {
            assert_eq!(e["transition"][field], Value::Null);
        }
    }
    let view = p.next_state.coordination.view().unwrap();
    assert!(view["claims"].get("bd-one").is_none());
    assert_eq!(view["reservations"]["rv-one"]["status"], "closed");
    assert_eq!(
        view["reservations"]["rv-one"]["token"],
        "eff-reserve-a-000000"
    );
    assert_eq!(view["reservations"]["rv-one"]["paths"], json!(["src/a.rs"]));
    assert_eq!(view["sessions"]["sess-a"]["token"], "eff-enroll-a-000000");
    assert_eq!(view["sessions"]["sess-a"]["status"], "revoked");
    value["command"]["action"]["reservations"] = json!({"rv-aaa":"extra"});
    rejects(
        &state,
        serde_json::from_value(value).unwrap(),
        "reservation_mismatch",
        "rv-aaa",
    );
}

#[test]
fn begin_path_conflict_leaves_task_claim_and_notes_unchanged() {
    let mut state = seeded(0);
    apply(
        &mut state,
        request(
            "reserve",
            "bob",
            "sess-b",
            json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-held","paths":["src"]}),
        ),
    );
    let begin: Request =
        serde_json::from_value(fixture()["scenarios"][1]["vectors"][0]["request"].clone()).unwrap();
    rejects(&state, begin, "path_conflict", "rv-held");
    let view = state.coordination.view().unwrap();
    assert_eq!(view["tasks"]["bd-one"]["status"], "open");
    assert_eq!(view["tasks"]["bd-one"]["notes"], json!([]));
    assert_eq!(view["claims"], json!({}));
    assert!(view["reservations"].get("rv-one").is_none());
}

struct GitFixture {
    td: TempDir,
}
impl GitFixture {
    fn new() -> Self {
        let f = Self {
            td: tempdir().unwrap(),
        };
        let remote = f.td.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q"]);
        for name in ["a", "b"] {
            let repo = f.td.path().join(name);
            fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
        }
        let t = f.transport("a", "root");
        let root = t
            .create_commit(
                None,
                &BTreeMap::from([(
                    "manifest.json".into(),
                    encode(&Manifest::lifecycle("st-lifecycle".into())).unwrap(),
                )]),
            )
            .unwrap();
        assert!(t.push(&root).unwrap().success);
        f
    }
    fn transport(&self, repo: &str, name: &str) -> GitTransport {
        GitTransport::open(
            &self.td.path().join(repo),
            "origin",
            "mote-state",
            &self.td.path().join("state").join(name).join("git"),
        )
        .unwrap()
    }
    fn client(&self, repo: &str, name: &str) -> Client {
        Client::connect(
            self.transport(repo, name),
            &self.td.path().join("state").join(name).join("cache"),
        )
        .unwrap()
    }
}
fn git(repo: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}
fn accepted(client: &Client, r: Request) -> Submission {
    let s = client.submit(r, 8).unwrap();
    assert_eq!(s.status, SubmissionStatus::Accepted, "{s:?}");
    s
}
#[test]
fn real_git_recovery_survives_cache_loss_and_fences_stale_holder() {
    let f = GitFixture::new();
    let a = f.client("a", "a");
    for seed in fixture()["scenarios"][2]["seed"].as_array().unwrap() {
        accepted(&a, serde_json::from_value(seed.clone()).unwrap());
    }
    let b = f.client("b", "b");
    let recovery: Request =
        serde_json::from_value(fixture()["scenarios"][2]["vectors"][0]["request"].clone()).unwrap();
    let receipt = accepted(&b, recovery.clone());
    let stale = a
        .submit(
            request(
                "stale-release",
                "alice",
                "sess-a",
                json!({"kind":"claim.release","id":"bd-one","token":"eff-claim-a-000000"}),
            ),
            8,
        )
        .unwrap();
    assert_eq!(stale.status, SubmissionStatus::Rejected);
    assert_eq!(
        stale.receipt.as_ref().unwrap().outcome.result["code"],
        "session_unavailable"
    );
    let expected = b.refresh().unwrap();
    drop(b);
    fs::remove_dir_all(f.td.path().join("state/b")).unwrap();
    let replacement = f.client("b", "replacement");
    let recovered = replacement.submit(recovery, 8).unwrap();
    assert!(recovered.recovered);
    assert_eq!(recovered.receipt, receipt.receipt);
    assert_eq!(replacement.refresh().unwrap().state, expected.state);
}
#[test]
fn real_git_simultaneous_next_claims_distinct_tasks_then_handoff_and_done() {
    let f = GitFixture::new();
    let a = f.client("a", "a");
    for seed in fixture()["scenarios"][0]["seed"].as_array().unwrap() {
        accepted(&a, serde_json::from_value(seed.clone()).unwrap());
    }
    accepted(
        &a,
        request(
            "create-two",
            "alice",
            "sess-a",
            json!({"kind":"task.create","id":"bd-two","title":"Two","body":"","priority":2,"assignee":null}),
        ),
    );
    let b = f.client("b", "b");
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let next = |id, actor, session| {
        request(
            id,
            actor,
            session,
            json!({"kind":"next","tags":[],"exclude":[],"reservation":null,"paths":[],"note":null}),
        )
    };
    let ar = next("next-a", "alice", "sess-a");
    let br = next("next-b", "bob", "sess-b");
    let ta = thread::spawn(move || {
        other.wait();
        accepted(&a, ar)
    });
    let tb = thread::spawn(move || {
        barrier.wait();
        accepted(&b, br)
    });
    let ra = ta.join().unwrap().receipt.unwrap();
    let rb = tb.join().unwrap().receipt.unwrap();
    assert_ne!(ra.outcome.result["task"], rb.outcome.result["task"]);
    let a = f.client("a", "continue-a");
    let task = ra.outcome.result["task"].as_str().unwrap();
    let view = a.refresh().unwrap().state.coordination.view().unwrap();
    let clock = &view["tasks"][task]["clock"]["status"];
    let handed = accepted(
        &a,
        request(
            "handoff-a",
            "alice",
            "sess-a",
            json!({"kind":"handoff","id":task,"expect_status":clock,"expect_claim":ra.outcome.result["claim_token"],"reservations":{},"to_session":"sess-b","disposition":"release","note":"continue on other machine"}),
        ),
    );
    let b = f.client("b", "continue-b");
    accepted(
        &b,
        request(
            "done-a",
            "bob",
            "sess-b",
            json!({"kind":"done","id":task,"expect_status":clock,"expect_claim":handed.receipt.unwrap().outcome.result["claim_token"],"reservations":{},"note":"complete"}),
        ),
    );
    let end = request(
        "end-b",
        "bob",
        "sess-b",
        json!({"kind":"session.end","expect_session":"eff-enroll-b-000000","reason":"session finished"}),
    );
    let ended = accepted(&b, end.clone());
    let recovered = b.submit(end, 8).unwrap();
    assert!(recovered.recovered);
    assert_eq!(recovered.receipt, ended.receipt);
    let view = b.refresh().unwrap().state.coordination.view().unwrap();
    assert_eq!(view["tasks"][task]["status"], "closed");
    assert_eq!(view["claims"], json!({}));
}
