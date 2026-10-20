//! Profile isolation and task/ownership behavior through real Git publication.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;

use mote::git_authority::client::{Client, Submission, SubmissionStatus};
use mote::git_authority::planner::AuthorityState;
use mote::git_authority::protocol::{
    Manifest, Request, Transaction, decode, encode, transaction_path,
};
use mote::git_authority::transport::GitTransport;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

struct Fixture {
    _td: TempDir,
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

impl Fixture {
    fn new(coordination: bool) -> Self {
        let td = tempdir().unwrap();
        let root = td.path().to_path_buf();
        let remote = root.join("remote.git");
        let a = root.join("a");
        let b = root.join("b");
        for path in [&remote, &a, &b] {
            fs::create_dir(path).unwrap();
        }
        git(&remote, &["init", "--bare", "-q"]);
        for path in [&a, &b] {
            git(path, &["init", "-q"]);
            git(path, &["remote", "add", "origin", remote.to_str().unwrap()]);
        }
        let f = Self {
            _td: td,
            root,
            a,
            b,
        };
        let transport = f.transport(&f.a, "bootstrap");
        let manifest = if coordination {
            Manifest::coordination("st-profile-test".into())
        } else {
            Manifest::new("st-profile-test".into())
        };
        let commit = transport
            .create_commit(
                None,
                &BTreeMap::from([("manifest.json".into(), encode(&manifest).unwrap())]),
            )
            .unwrap();
        assert!(transport.push(&commit).unwrap().success);
        f
    }

    fn transport(&self, repo: &Path, name: &str) -> GitTransport {
        GitTransport::open(
            repo,
            "origin",
            "mote-state",
            &self.root.join("clients").join(name).join("git"),
        )
        .unwrap()
    }

    fn client(&self, repo: &Path, name: &str) -> Client {
        Client::connect(
            self.transport(repo, name),
            &self.root.join("clients").join(name).join("cache"),
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

fn request(id: &str, session: &str, action: Value) -> Request {
    serde_json::from_value(json!({
        "version":1,"request_id":id,"actor":"alice","session":session,
        "timestamp":"2026-10-20T00:00:00Z",
        "command":{"kind":"coordination","action":action}
    }))
    .unwrap()
}

fn synthetic() -> Request {
    serde_json::from_value(json!({
        "version":1,"request_id":"synthetic","actor":"alice","session":"sess-a",
        "timestamp":"2026-10-20T00:00:00Z",
        "command":{"kind":"synthetic.cas","changes":[{"key":"task","expected":null,"value":"alice"}]}
    })).unwrap()
}

fn submit(client: &Client, id: &str, session: &str, action: Value) -> Submission {
    client.submit(request(id, session, action), 8).unwrap()
}

fn enroll(client: &Client, id: &str, session: &str) {
    assert_eq!(
        submit(
            client,
            id,
            session,
            json!({"kind":"session.start","label":session})
        )
        .status,
        SubmissionStatus::Accepted
    );
}

fn create(client: &Client, id: &str, task: &str) -> Submission {
    let s = submit(
        client,
        id,
        "sess-a",
        json!({"kind":"task.create","id":task,"title":"work","body":"","priority":2,"assignee":null}),
    );
    assert_eq!(s.status, SubmissionStatus::Accepted);
    s
}

fn race(a: Client, b: Client, first: Request, second: Request) -> [Submission; 2] {
    let barrier = Arc::new(Barrier::new(2));
    let other = barrier.clone();
    let one = thread::spawn(move || {
        other.wait();
        a.submit(first, 8).unwrap()
    });
    let two = thread::spawn(move || {
        barrier.wait();
        b.submit(second, 8).unwrap()
    });
    [one.join().unwrap(), two.join().unwrap()]
}

fn one_winner(results: &[Submission; 2]) {
    assert_eq!(
        results
            .iter()
            .filter(|s| s.status == SubmissionStatus::Accepted)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|s| s.status == SubmissionStatus::Rejected)
            .count(),
        1
    );
}

#[test]
fn independent_coordination_vectors_pin_effect_bytes_receipts_and_state() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/git_authority/coordination.json")).unwrap();
    let mut state = AuthorityState::default();
    for vector in fixtures["vectors"].as_array().unwrap() {
        let r: Request = decode(&serde_json::to_vec(&vector["request"]).unwrap()).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&encode(&r).unwrap()).unwrap(),
            vector["request"]
        );
        let before = state.coordination.clone();
        let before_entries = serde_json::to_value(&before).unwrap()["task_entries"].clone();
        let planned = mote::git_authority::planner::plan(&state, &r).unwrap();
        assert_eq!(
            serde_json::to_value(&planned.effects).unwrap(),
            vector["effects"],
            "{}",
            vector["name"]
        );
        assert_eq!(
            serde_json::to_value(&planned.outcome).unwrap(),
            vector["outcome"],
            "{}",
            vector["name"]
        );
        let tx = Transaction::prepare(&state, &"1".repeat(40), r).unwrap();
        assert_eq!(tx.validate(&state, &tx.parent).unwrap(), planned.next_state);
        state = planned.next_state;
        assert_eq!(state.sequence, vector["sequence"].as_u64().unwrap());
        if vector["outcome"]["status"] == "rejected" {
            assert_eq!(state.coordination, before);
        }
        if vector["effects"][0]["kind"] == "ownership" {
            assert_eq!(
                serde_json::to_value(&state.coordination).unwrap()["task_entries"],
                before_entries,
                "ownership changes cannot create task history"
            );
        }
        let view = state.coordination.view().unwrap();
        let task = &view["tasks"]["bd-one"];
        match state.sequence {
            2 => {
                let clock = "gaop-00000000000000000002-000000-eff-create-000000";
                for field in ["title", "body", "priority", "status"] {
                    assert_eq!(task["clock"][field], clock);
                }
                assert!(task["clock"].get("assignee").is_none());
            }
            4 => {
                assert_eq!(task["body"], "Body updated");
                assert_eq!(
                    task["clock"]["body"],
                    "gaop-00000000000000000003-000000-eff-edit-body-000000"
                );
                assert_eq!(
                    task["clock"]["title"],
                    "gaop-00000000000000000004-000000-eff-edit-title-000000"
                );
            }
            7 | 8 | 12 => {
                assert_eq!(view["claims"]["bd-one"]["session"], "sess-a");
                assert_eq!(view["claims"]["bd-one"]["token"], "eff-claim-a-000000");
            }
            _ => {}
        }
        if state.sequence == 8 {
            assert_eq!(
                view["sessions"]["sess-a"]["presence_timestamp"],
                "2026-10-19T00:00:00Z"
            );
            assert_eq!(view["sessions"]["sess-a"]["token"], "eff-enroll-000000");
        }
        if state.sequence == 12 {
            assert_eq!(view["reservations"]["rv-one"]["status"], "closed");
            assert_eq!(
                view["reservations"]["rv-one"]["token"],
                "eff-reserve-000000"
            );
            assert_eq!(view["reservations"]["rv-one"]["paths"], json!(["src/a.rs"]));
        }
        if state.sequence == 13 {
            assert_eq!(task["assignee"], "bob");
            assert_eq!(
                task["clock"]["assignee"],
                "gaop-00000000000000000013-000000-eff-assign-000000"
            );
        }
        if state.sequence == 15 {
            assert_eq!(view["tasks"]["bd-two"]["assignee"], "charlie");
            assert_eq!(view["tasks"]["bd-two"]["title"], "Mixed title");
        }
    }
}

#[test]
fn core_syntax_rejects_missing_nullable_fields_and_unqualified_actions() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/git_authority/coordination.json")).unwrap();
    let create = &fixture["vectors"][1]["request"];
    let mut omitted = create.clone();
    omitted["command"]["action"]
        .as_object_mut()
        .unwrap()
        .remove("assignee");
    assert!(decode::<Request>(&serde_json::to_vec(&omitted).unwrap()).is_err());
    for action in [
        json!({"kind":"task.patch","id":"bd-one","set":{"title":"a"},"expect":{}}),
        json!({"kind":"task.patch","id":"bd-one","set":{"status":"closed"},"expect":{"status":null}}),
        json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-one","paths":["src//a.rs"]}),
        json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-one","paths":["../secret"]}),
        json!({"kind":"session.end"}),
        json!({"kind":"begin","id":"bd-one"}),
        json!({"kind":"task.op","op":{"kind":"claim","ttl":60}}),
    ] {
        let mut value = create.clone();
        value["command"]["action"] = action;
        assert!(
            decode::<Request>(&serde_json::to_vec(&value).unwrap())
                .and_then(|r| r.validate())
                .is_err(),
            "must refuse {}",
            value["command"]["action"]
        );
    }
}

fn pure_step(state: &mut AuthorityState, id: &str, session: &str, action: Value) -> Value {
    let planned = mote::git_authority::planner::plan(state, &request(id, session, action)).unwrap();
    let before = state.coordination.clone();
    let result = serde_json::to_value(&planned.outcome).unwrap();
    if result["status"] == "rejected" {
        assert_eq!(
            planned.next_state.coordination, before,
            "denied actions must preserve values and task history"
        );
    }
    *state = planned.next_state;
    result
}

#[test]
fn graph_rules_tags_and_note_provenance_follow_task_semantics() {
    let mut state = AuthorityState::default();
    pure_step(
        &mut state,
        "start",
        "sess-a",
        json!({"kind":"session.start","label":"test"}),
    );
    for id in ["bd-one", "bd-two", "bd-three"] {
        pure_step(
            &mut state,
            id,
            "sess-a",
            json!({"kind":"task.create","id":id,"title":id,"body":"","priority":2,"assignee":null}),
        );
    }
    for (id, parent, rid) in [
        ("bd-one", "bd-two", "dep-one"),
        ("bd-two", "bd-three", "dep-two"),
    ] {
        assert_eq!(
            pure_step(
                &mut state,
                rid,
                "sess-a",
                json!({"kind":"task.dependency","id":id,"parent":parent,"add":true})
            )["status"],
            "accepted"
        );
    }
    let denied = pure_step(
        &mut state,
        "dep-cycle",
        "sess-a",
        json!({"kind":"task.dependency","id":"bd-three","parent":"bd-one","add":true}),
    );
    assert_eq!(
        denied["result"],
        json!({"code":"cycle","subject":"bd-three"})
    );
    pure_step(
        &mut state,
        "claim",
        "sess-a",
        json!({"kind":"claim.acquire","id":"bd-three"}),
    );
    assert_eq!(
        pure_step(
            &mut state,
            "held-cycle",
            "sess-a",
            json!({"kind":"task.dependency","id":"bd-three","parent":"bd-one","add":true})
        )["result"],
        json!({"code":"held_work","subject":"bd-three"})
    );
    pure_step(
        &mut state,
        "parent-one",
        "sess-a",
        json!({"kind":"task.relation","id":"bd-one","parent":"bd-two","relation":"parent","add":true}),
    );
    assert_eq!(
        pure_step(
            &mut state,
            "parent-cycle",
            "sess-a",
            json!({"kind":"task.relation","id":"bd-two","parent":"bd-one","relation":"parent","add":true})
        )["result"],
        json!({"code":"cycle","subject":"bd-two"})
    );
    for (id, parent, rid) in [
        ("bd-one", "bd-two", "related-one"),
        ("bd-two", "bd-one", "related-two"),
    ] {
        assert_eq!(
            pure_step(
                &mut state,
                rid,
                "sess-a",
                json!({"kind":"task.relation","id":id,"parent":parent,"relation":"related","add":true})
            )["status"],
            "accepted"
        );
    }
    assert_eq!(
        pure_step(
            &mut state,
            "self",
            "sess-a",
            json!({"kind":"task.relation","id":"bd-one","parent":"bd-one","relation":"related","add":true})
        )["result"],
        json!({"code":"self_edge","subject":"bd-one"})
    );
    assert_eq!(
        pure_step(
            &mut state,
            "missing-both",
            "sess-a",
            json!({"kind":"task.dependency","id":"bd-missing","parent":"bd-absent","add":true})
        )["result"],
        json!({"code":"task_unavailable","subject":"bd-missing"})
    );
    assert_eq!(
        pure_step(
            &mut state,
            "missing-parent",
            "sess-a",
            json!({"kind":"task.dependency","id":"bd-one","parent":"bd-absent","add":true})
        )["result"],
        json!({"code":"task_unavailable","subject":"bd-absent"})
    );
    for (rid, add) in [
        ("add-tag", true),
        ("remove-tag", false),
        ("absent-tag", false),
    ] {
        assert_eq!(
            pure_step(
                &mut state,
                rid,
                "sess-a",
                json!({"kind":"task.tag","id":"bd-one","tag":"qa","add":add})
            )["status"],
            "accepted"
        );
    }
    for kind in [
        "note", "progress", "decision", "design", "handoff", "blocker",
    ] {
        assert_eq!(
            pure_step(
                &mut state,
                kind,
                "sess-a",
                json!({"kind":"task.note","id":"bd-one","note_kind":kind,"text":"retained text"})
            )["status"],
            "accepted"
        );
    }
    let view = state.coordination.view().unwrap();
    assert_eq!(view["tasks"]["bd-one"]["tags"], json!([]));
    let notes = view["tasks"]["bd-one"]["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 6);
    for note in notes {
        assert_eq!(note["actor"], "alice");
        assert_eq!(note["ts"], "2026-10-20T00:00:00Z");
        assert_eq!(note["text"], "retained text");
        assert!(note["op_id"].as_str().unwrap().starts_with("gaop-"));
    }
}

#[test]
fn ownership_generation_and_reused_reservation_precedence_are_exact() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/git_authority/coordination.json")).unwrap();
    let mut state = AuthorityState::default();
    for vector in fixtures["vectors"].as_array().unwrap() {
        let r: Request = serde_json::from_value(vector["request"].clone()).unwrap();
        state = mote::git_authority::planner::plan(&state, &r)
            .unwrap()
            .next_state;
    }
    assert_eq!(
        pure_step(
            &mut state,
            "foreign-release",
            "sess-b",
            json!({"kind":"claim.release","id":"bd-one","token":"eff-claim-a-000000"})
        )["result"],
        json!({"code":"claim_mismatch","subject":"bd-one"})
    );
    assert_eq!(
        pure_step(
            &mut state,
            "own-repeat",
            "sess-a",
            json!({"kind":"claim.acquire","id":"bd-one"})
        )["result"],
        json!({"code":"claim_occupied","subject":"bd-one"})
    );
    assert_eq!(
        pure_step(
            &mut state,
            "release-claim",
            "sess-a",
            json!({"kind":"claim.release","id":"bd-one","token":"eff-claim-a-000000"})
        )["status"],
        "accepted"
    );
    assert_eq!(
        pure_step(
            &mut state,
            "new-generation",
            "sess-a",
            json!({"kind":"claim.acquire","id":"bd-one"})
        )["status"],
        "accepted"
    );
    assert_eq!(
        pure_step(
            &mut state,
            "stale-release",
            "sess-a",
            json!({"kind":"claim.release","id":"bd-one","token":"eff-claim-a-000000"})
        )["result"],
        json!({"code":"claim_mismatch","subject":"bd-one"})
    );
    pure_step(
        &mut state,
        "reserve-current",
        "sess-a",
        json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-current","paths":["src/"]}),
    );
    assert_eq!(
        pure_step(
            &mut state,
            "reused-and-overlap",
            "sess-a",
            json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-one","paths":["src/a.rs"]})
        )["result"],
        json!({"code":"reservation_exists","subject":"rv-one"})
    );
    let view = state.coordination.view().unwrap();
    assert_eq!(
        view["claims"]["bd-one"]["token"],
        "eff-new-generation-000000"
    );
    assert_eq!(
        pure_step(
            &mut state,
            "stale-assignee",
            "sess-a",
            json!({"kind":"task.patch","id":"bd-one","set":{"assignee":"carol"},"expect":{"assignee":null}})
        )["result"],
        json!({"code":"stale_field","subject":"assignee"})
    );
    assert_eq!(
        pure_step(
            &mut state,
            "two-stale-fields",
            "sess-a",
            json!({"kind":"task.patch","id":"bd-one","set":{"title":"a","body":"b"},"expect":{"title":"wrong","body":"wrong"}})
        )["result"],
        json!({"code":"stale_field","subject":"body"})
    );
}

#[test]
fn public_projection_refuses_legacy_ownership_or_rejected_task_entry_passthrough() {
    use mote::git_authority::coordination::CoordinationState;
    for op in [
        json!({"kind":"claim","v":1,"op":"gaop-00000000000000000001-000000-eff-injected-000000","ts":"2026-10-20T00:00:00Z","actor":"alice","entity":"bd-one","to":"alice","ttl_s":60}),
        json!({"kind":"patch","v":1,"op":"gaop-00000000000000000001-000000-eff-injected-000000","ts":"2026-10-20T00:00:00Z","actor":"alice","entity":"bd-missing","expect":{},"set":{"body":"hidden rejection"}}),
    ] {
        let mut value = serde_json::to_value(CoordinationState::default()).unwrap();
        value["task_entries"] = json!([op]);
        let malformed: CoordinationState = serde_json::from_value(value).unwrap();
        assert!(malformed.view().is_err());
        assert!(malformed.task_view().is_err());
    }
}

#[test]
fn immutable_profile_is_checked_before_submission_and_during_history_replay() {
    for coordination in [false, true] {
        let f = Fixture::new(coordination);
        let client = f.client(&f.a, "a");
        let wrong = if coordination {
            synthetic()
        } else {
            request(
                "enroll",
                "sess-a",
                json!({"kind":"session.start","label":"laptop"}),
            )
        };
        assert!(client.submit(wrong.clone(), 2).is_err());
        let snapshot = client.refresh().unwrap();
        assert_eq!(snapshot.state, AuthorityState::default());
        let previous_view = client.cached_view().unwrap().unwrap();
        // A cooperating client refuses this request; direct Git access still
        // permits a malformed append, which every reader must reject.
        let tx = Transaction::prepare(&snapshot.state, &snapshot.head, wrong).unwrap();
        let mut files = snapshot.files;
        files.insert(transaction_path(tx.sequence), encode(&tx).unwrap());
        let transport = f.transport(&f.a, "malformed");
        assert_eq!(
            transport.head().unwrap().as_deref(),
            Some(snapshot.head.as_str())
        );
        let commit = transport
            .create_commit(Some(&snapshot.head), &files)
            .unwrap();
        assert!(transport.push(&commit).unwrap().success);
        assert!(client.refresh().is_err());
        assert_eq!(
            client.cached_view().unwrap().unwrap().head,
            previous_view.head
        );
        assert!(Client::connect(f.transport(&f.b, "fresh"), &f.root.join("fresh-cache")).is_err());
    }
}

#[test]
fn same_actor_separate_machine_sessions_cannot_both_acquire_the_task() {
    let f = Fixture::new(true);
    let a = f.client(&f.a, "a");
    let b = f.client(&f.b, "b");
    enroll(&a, "enroll-a", "sess-a");
    enroll(&b, "enroll-b", "sess-b");
    create(&a, "create", "bd-one");
    let result = race(
        a,
        b,
        request(
            "claim-a",
            "sess-a",
            json!({"kind":"claim.acquire","id":"bd-one"}),
        ),
        request(
            "claim-b",
            "sess-b",
            json!({"kind":"claim.acquire","id":"bd-one"}),
        ),
    );
    one_winner(&result);
    let a = f.client(&f.a, "a").refresh().unwrap();
    let b = f.client(&f.b, "b").refresh().unwrap();
    assert_eq!(a.state, b.state);
    assert_eq!(a.receipts, b.receipts);
    assert_eq!(a.receipts.len(), 5);
    assert!(a.state.values.is_empty());
}

#[test]
fn disjoint_field_races_succeed_and_same_field_races_keep_original_expectations() {
    let f = Fixture::new(true);
    let a = f.client(&f.a, "a");
    enroll(&a, "enroll-a", "sess-a");
    enroll(&a, "enroll-b", "sess-b");
    let made = create(&a, "create", "bd-one");
    let initial_clock = format!(
        "gaop-{:020}-000000-eff-create-000000",
        made.receipt.unwrap().sequence
    );
    let result = race(
        a,
        f.client(&f.b, "b"),
        request(
            "title",
            "sess-a",
            json!({"kind":"task.patch","id":"bd-one","set":{"title":"new title"},"expect":{"title":initial_clock}}),
        ),
        request(
            "body",
            "sess-b",
            json!({"kind":"task.patch","id":"bd-one","set":{"body":"details"},"expect":{"body":initial_clock}}),
        ),
    );
    assert!(
        result
            .iter()
            .all(|s| s.status == SubmissionStatus::Accepted)
    );
    let title_sequence = result[0].receipt.as_ref().unwrap().sequence;
    let title_clock = format!("gaop-{title_sequence:020}-000000-eff-title-000000");
    let result = race(
        f.client(&f.a, "a"),
        f.client(&f.b, "b"),
        request(
            "title-a",
            "sess-a",
            json!({"kind":"task.patch","id":"bd-one","set":{"title":"a"},"expect":{"title":title_clock}}),
        ),
        request(
            "title-b",
            "sess-b",
            json!({"kind":"task.patch","id":"bd-one","set":{"title":"b"},"expect":{"title":title_clock}}),
        ),
    );
    one_winner(&result);
    let loser = result
        .iter()
        .find(|s| s.status == SubmissionStatus::Rejected)
        .unwrap();
    assert_eq!(
        loser.receipt.as_ref().unwrap().outcome.result,
        json!({"code":"stale_field","subject":"title"})
    );
}

#[test]
fn prefix_reservations_conflict_across_tasks_and_reversed_presence_does_not_expire_them() {
    let f = Fixture::new(true);
    let a = f.client(&f.a, "a");
    enroll(&a, "enroll-a", "sess-a");
    enroll(&a, "enroll-b", "sess-b");
    create(&a, "create-one", "bd-one");
    create(&a, "create-two", "bd-two");
    let result = race(
        a,
        f.client(&f.b, "b"),
        request(
            "reserve-a",
            "sess-a",
            json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-one","paths":["src/"]}),
        ),
        request(
            "reserve-b",
            "sess-b",
            json!({"kind":"reservation.acquire","id":"bd-two","reservation":"rv-two","paths":["src/a.rs"]}),
        ),
    );
    one_winner(&result);
    let client = f.client(&f.a, "a");
    let mut presence = request(
        "past",
        "sess-a",
        json!({"kind":"session.presence","text":"backwards clock"}),
    );
    presence.timestamp = "2020-01-01T00:00:00Z".into();
    assert_eq!(
        client.submit(presence, 3).unwrap().status,
        SubmissionStatus::Accepted
    );
    let same_session = submit(
        &client,
        "still-held",
        "sess-a",
        json!({"kind":"reservation.acquire","id":"bd-one","reservation":"rv-third","paths":["src/a.rs"]}),
    );
    assert_eq!(same_session.status, SubmissionStatus::Rejected);
    assert_eq!(
        same_session.receipt.unwrap().outcome.result["code"],
        "path_conflict"
    );
    let cached = client.cached_view().unwrap().unwrap();
    assert!(!cached.fresh);
    assert_eq!(
        cached.values,
        client.refresh().unwrap().state.coordination.view().unwrap()
    );
}

#[test]
fn task_and_note_receipts_survive_loss_of_the_submitting_cache() {
    let f = Fixture::new(true);
    let client = f.client(&f.a, "original");
    enroll(&client, "enroll-a", "sess-a");
    let made = create(&client, "create", "bd-one");
    let assign = request(
        "assign",
        "sess-a",
        json!({"kind":"task.patch","id":"bd-one","set":{"assignee":"bob","title":"assigned work"},"expect":{"assignee":null,"title":"gaop-00000000000000000002-000000-eff-create-000000"}}),
    );
    let assigned = client.submit(assign.clone(), 3).unwrap();
    assert_eq!(assigned.status, SubmissionStatus::Accepted);
    let r = request(
        "note",
        "sess-a",
        json!({"kind":"task.note","id":"bd-one","note_kind":"note","text":"one retained note"}),
    );
    let note = client.submit(r.clone(), 3).unwrap();
    assert_eq!(note.status, SubmissionStatus::Accepted);
    let expected = client.refresh().unwrap();
    drop(client);
    fs::remove_dir_all(f.root.join("clients/original")).unwrap();
    let fresh = f.client(&f.b, "replacement");
    let recovered = fresh.submit(r, 3).unwrap();
    assert!(recovered.recovered);
    assert_eq!(recovered.receipt, note.receipt);
    assert_eq!(fresh.receipt("create").unwrap(), made.receipt);
    let recovered_assignment = fresh.submit(assign, 3).unwrap();
    assert!(recovered_assignment.recovered);
    assert_eq!(recovered_assignment.receipt, assigned.receipt);
    let actual = fresh.refresh().unwrap();
    assert_eq!(actual.head, expected.head);
    assert_eq!(actual.state, expected.state);
    assert_eq!(actual.receipts.len(), 4);
}
