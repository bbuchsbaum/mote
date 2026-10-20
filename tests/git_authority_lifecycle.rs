use mote::git_authority::{
    coordination::{
        self, Action, CoordinationState,
        lifecycle::{Begin, Next},
    },
    protocol::{Command, OutcomeStatus, Request},
};

fn request(id: &str, action: Action) -> Request {
    Request {
        version: 1,
        request_id: id.into(),
        actor: "alice".into(),
        session: "sess-a".into(),
        timestamp: "2026-10-20T00:00:00Z".into(),
        command: Command::Coordination { action },
    }
}

fn apply(state: &mut CoordinationState, seq: u64, request: Request) {
    let plan = coordination::plan(state, &request, seq).unwrap();
    assert_eq!(plan.outcome.status, OutcomeStatus::Accepted);
    *state = plan.next_state;
}

#[test]
fn begin_path_conflict_is_atomic_and_next_no_ready_has_a_receipt() {
    let mut state = CoordinationState::default();
    apply(
        &mut state,
        1,
        request("start", Action::SessionStart { label: "a".into() }),
    );
    apply(
        &mut state,
        2,
        request(
            "create",
            Action::TaskCreate {
                id: "bd-one".into(),
                title: "one".into(),
                body: "".into(),
                priority: 0,
                assignee: None,
            },
        ),
    );
    apply(
        &mut state,
        3,
        request(
            "hold",
            Action::ReservationAcquire {
                id: "bd-one".into(),
                reservation: "rv-held".into(),
                paths: vec!["src".into()],
            },
        ),
    );
    let before = state.clone();
    let rejected = coordination::plan(
        &state,
        &request(
            "begin",
            Action::Begin(Begin {
                id: "bd-one".into(),
                expect_status: "missing".into(),
                reservation: Some("rv-new".into()),
                paths: vec!["src/a.rs".into()],
                note: Some("start".into()),
            }),
        ),
        4,
    )
    .unwrap();
    assert_eq!(rejected.outcome.result["code"], "stale_field");
    assert!(rejected.effects.is_empty());
    assert_eq!(rejected.next_state, before);

    let no_ready = coordination::plan(
        &state,
        &request(
            "next",
            Action::Next(Next {
                tags: vec![],
                exclude: vec![],
                reservation: None,
                paths: vec![],
                note: None,
            }),
        ),
        4,
    )
    .unwrap();
    assert_eq!(no_ready.outcome.status, OutcomeStatus::Rejected);
    assert_eq!(no_ready.outcome.result["code"], "no_ready_task");
    assert_eq!(no_ready.outcome.result["subject"], "next");
    assert!(no_ready.effects.is_empty());
}
