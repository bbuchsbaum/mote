use mote::git_authority::{
    coordination::{self, Action, CoordinationState},
    protocol::{Command, OutcomeStatus, Request},
};
use std::collections::BTreeMap;

fn req(id: &str, session: &str, actor: &str, action: Action) -> Request {
    Request {
        version: 1,
        request_id: id.into(),
        actor: actor.into(),
        session: session.into(),
        timestamp: "2026-10-20T00:00:00Z".into(),
        command: Command::Coordination { action },
    }
}
fn step(
    state: &mut CoordinationState,
    n: &mut u64,
    r: Request,
) -> mote::git_authority::coordination::CoordinationPlan {
    *n += 1;
    let p = coordination::plan(state, &r, *n).unwrap();
    *state = p.next_state.clone();
    p
}

#[test]
fn sessions_create_patch_clocks_and_same_actor_distinct_sessions() {
    let (mut s, mut n) = (CoordinationState::default(), 0);
    assert_eq!(
        step(
            &mut s,
            &mut n,
            req(
                "start-a",
                "sess-a",
                "alice",
                Action::SessionStart { label: "a".into() }
            )
        )
        .outcome
        .status,
        OutcomeStatus::Accepted
    );
    step(
        &mut s,
        &mut n,
        req(
            "start-b",
            "sess-b",
            "alice",
            Action::SessionStart { label: "b".into() },
        ),
    );
    step(
        &mut s,
        &mut n,
        req(
            "create",
            "sess-a",
            "alice",
            Action::TaskCreate {
                id: "bd-one".into(),
                title: "one".into(),
                body: "".into(),
                priority: 1,
                assignee: None,
            },
        ),
    );
    let p = step(
        &mut s,
        &mut n,
        req(
            "patch",
            "sess-b",
            "alice",
            Action::TaskPatch {
                id: "bd-one".into(),
                set: BTreeMap::from([("title".into(), serde_json::json!("two"))]),
                expect: BTreeMap::from([(
                    "title".into(),
                    Some("gaop-00000000000000000003-000000-eff-create-000000".into()),
                )]),
            },
        ),
    );
    assert_eq!(p.outcome.status, OutcomeStatus::Accepted);
    let stale = coordination::plan(
        &s,
        &req(
            "stale",
            "sess-a",
            "alice",
            Action::TaskPatch {
                id: "bd-one".into(),
                set: BTreeMap::from([("title".into(), serde_json::json!("three"))]),
                expect: BTreeMap::from([("title".into(), None)]),
            },
        ),
        n + 1,
    )
    .unwrap();
    assert_eq!(stale.outcome.result["code"], "stale_field");
}
#[test]
fn exact_tokens_reservations_and_graph_precedence() {
    let (mut s, mut n) = (CoordinationState::default(), 0);
    step(
        &mut s,
        &mut n,
        req("s", "s", "a", Action::SessionStart { label: "x".into() }),
    );
    for id in ["bd-a", "bd-b"] {
        step(
            &mut s,
            &mut n,
            req(
                &format!("c{id}"),
                "s",
                "a",
                Action::TaskCreate {
                    id: id.into(),
                    title: id.into(),
                    body: "".into(),
                    priority: 0,
                    assignee: None,
                },
            ),
        );
    }
    let claim = step(
        &mut s,
        &mut n,
        req(
            "claim",
            "s",
            "a",
            Action::ClaimAcquire { id: "bd-a".into() },
        ),
    );
    let token = claim.effects[0].effect_id().to_string();
    let wrong = coordination::plan(
        &s,
        &req(
            "wrong",
            "s",
            "a",
            Action::ClaimRelease {
                id: "bd-a".into(),
                token: "bad".into(),
            },
        ),
        n + 1,
    )
    .unwrap();
    assert_eq!(wrong.outcome.result["code"], "claim_mismatch");
    step(
        &mut s,
        &mut n,
        req(
            "release",
            "s",
            "a",
            Action::ClaimRelease {
                id: "bd-a".into(),
                token,
            },
        ),
    );
    step(
        &mut s,
        &mut n,
        req(
            "reserve",
            "s",
            "a",
            Action::ReservationAcquire {
                id: "bd-a".into(),
                reservation: "rv-a".into(),
                paths: vec!["src/a".into()],
            },
        ),
    );
    let conflict = coordination::plan(
        &s,
        &req(
            "reserve2",
            "s",
            "a",
            Action::ReservationAcquire {
                id: "bd-b".into(),
                reservation: "rv-b".into(),
                paths: vec!["src/a/x".into()],
            },
        ),
        n + 1,
    )
    .unwrap();
    assert_eq!(conflict.outcome.result["code"], "path_conflict");
    let dep = coordination::plan(
        &s,
        &req(
            "dep",
            "s",
            "a",
            Action::TaskDependency {
                id: "bd-a".into(),
                parent: "bd-b".into(),
                add: true,
            },
        ),
        n + 1,
    )
    .unwrap();
    assert_eq!(dep.outcome.result["code"], "held_work");
}

#[test]
fn syntax_rejects_empty_titles_and_plan_validates_the_full_request_envelope() {
    let empty_create = req(
        "empty-create",
        "s",
        "a",
        Action::TaskCreate {
            id: "bd-one".into(),
            title: "".into(),
            body: "".into(),
            priority: 0,
            assignee: None,
        },
    );
    assert!(empty_create.validate().is_err());
    let empty_patch = req(
        "empty-patch",
        "s",
        "a",
        Action::TaskPatch {
            id: "bd-one".into(),
            set: BTreeMap::from([("title".into(), serde_json::json!(""))]),
            expect: BTreeMap::from([("title".into(), None)]),
        },
    );
    assert!(empty_patch.validate().is_err());
    let mut invalid = req(
        "bad-envelope",
        "s",
        "a",
        Action::SessionStart { label: "ok".into() },
    );
    invalid.version = 2;
    assert!(coordination::plan(&CoordinationState::default(), &invalid, 1).is_err());
}
