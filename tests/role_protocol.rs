use std::path::Path;
use std::process::Command;

use jiff::Timestamp;
use tempfile::TempDir;

use mote::actor_status;
use mote::audit::{AuditFailOn, run_audit};
use mote::ids;
use mote::op::{
    Op, RoleAssignOp, RoleDefineOp, RoleReleaseOp, RoleRenewOp, RoleRetireOp, make_session_end,
    make_session_heartbeat, make_session_start,
};
use mote::role::{
    ROLE_PROTOCOL_VERSION, RoleAssignmentClock, RoleAssignmentDisposition, RoleExclusion,
    RoleExclusionCode,
};
use mote::{publish, reducer, repo::Store};

fn timestamp(value: &str) -> Timestamp {
    value.parse().unwrap()
}

fn setup() -> (TempDir, Store) {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    (temp, store)
}

fn publish_checked(store: &Store, operation: &Op) -> String {
    let name = publish::publish_op(store, operation).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(
        state.was_accepted(name.as_str()),
        "rejected: {:?}",
        state.rejection_reason(name.as_str())
    );
    name.into_string()
}

fn publish_rejected(store: &Store, operation: &Op, fragment: &str) -> String {
    let name = publish::publish_op(store, operation).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(!state.was_accepted(name.as_str()));
    let reason = state.rejection_reason(name.as_str()).unwrap();
    assert!(
        reason.contains(fragment),
        "expected `{fragment}` in `{reason}`"
    );
    name.into_string()
}

fn start_session(store: &Store, actor: &str, session_id: &str, at: &str, ttl_s: u32) -> String {
    publish_checked(
        store,
        &make_session_start(
            actor.into(),
            session_id.into(),
            ttl_s,
            None,
            None,
            timestamp(at),
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn define_role(
    role_id: &str,
    name: &str,
    actor: &str,
    capacity: u32,
    minimum_active: u32,
    exclusions: Vec<RoleExclusion>,
    at: &str,
    key: &str,
) -> Op {
    let mut authorities = vec![actor.to_string()];
    authorities.sort();
    Op::RoleDefine(RoleDefineOp {
        v: ROLE_PROTOCOL_VERSION,
        op: String::new(),
        ts: at.into(),
        actor: actor.into(),
        role_id: role_id.into(),
        name: name.into(),
        remit: format!("{name} remit"),
        exclusions,
        assignment_authorities: authorities,
        capacity,
        minimum_active,
        idempotency_key: key.into(),
    })
}

#[allow(clippy::too_many_arguments)]
fn assign_role(
    role_id: &str,
    definition_op: &str,
    assignment_id: &str,
    authority: &str,
    holder: &str,
    session_id: &str,
    session_op: &str,
    ttl_s: u32,
    expect_active: Vec<RoleAssignmentClock>,
    at: &str,
    key: &str,
) -> Op {
    Op::RoleAssign(RoleAssignOp {
        v: ROLE_PROTOCOL_VERSION,
        op: String::new(),
        ts: at.into(),
        actor: authority.into(),
        role_id: role_id.into(),
        expect_definition: definition_op.into(),
        assignment_id: assignment_id.into(),
        holder_actor: holder.into(),
        holder_session_id: session_id.into(),
        expect_session: session_op.into(),
        ttl_s,
        expect_active,
        idempotency_key: key.into(),
    })
}

#[test]
fn assignment_is_session_bounded_and_vacancy_is_explicit() {
    let (temp, store) = setup();
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "reviewer",
            "chief",
            2,
            2,
            Vec::new(),
            "2030-01-01T00:00:00.000000Z",
            "define-reviewer",
        ),
    );
    let session_id = ids::new_session_id();
    let session_op = start_session(&store, "alice", &session_id, "2030-01-01T00:00:01Z", 3_600);
    let state = reducer::replay_store(&store).unwrap();
    let vacant = state
        .role_coverage(&role_id, "2030-01-01T00:00:02.000000Z")
        .unwrap();
    assert!(vacant.vacant);
    assert_eq!(vacant.coverage_shortfall, 2);

    let assignment_id = ids::new_role_assignment_id();
    publish_checked(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &assignment_id,
            "chief",
            "alice",
            &session_id,
            &session_op,
            600,
            Vec::new(),
            "2030-01-01T00:00:03.000000Z",
            "assign-alice",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.role_assignment_disposition(
            &state.role_assignments[&assignment_id],
            "2030-01-01T00:00:02.000000Z"
        ),
        RoleAssignmentDisposition::NotStarted
    );
    assert!(
        state
            .role_active_assignments(&role_id, "2030-01-01T00:00:02.000000Z")
            .is_empty()
    );
    let staffed = state
        .role_coverage(&role_id, "2030-01-01T00:00:04.000000Z")
        .unwrap();
    assert_eq!(staffed.active_count, 1);
    assert_eq!(staffed.coverage_shortfall, 1);
    assert_eq!(
        state.role_assignment_disposition(
            &state.role_assignments[&assignment_id],
            "2030-01-01T00:00:04.000000Z"
        ),
        RoleAssignmentDisposition::Active
    );
    let staffed_report = run_audit(
        &store,
        &state,
        temp.path(),
        timestamp("2030-01-01T00:00:04Z"),
        None,
        None,
        AuditFailOn::Never,
    )
    .unwrap();
    let staffed_finding = staffed_report
        .findings
        .iter()
        .find(|finding| finding.code == "role_coverage_shortfall")
        .unwrap();
    assert_eq!(
        staffed_finding.evidence["nearest_active_deadline_ts"],
        "2030-01-01T00:10:03.000000Z"
    );

    let expired_at = "2030-01-01T00:10:04.000000Z";
    assert_eq!(
        state.role_assignment_disposition(&state.role_assignments[&assignment_id], expired_at),
        RoleAssignmentDisposition::Expired
    );
    assert_eq!(
        state
            .role_coverage(&role_id, expired_at)
            .unwrap()
            .coverage_shortfall,
        2
    );

    let report = run_audit(
        &store,
        &state,
        temp.path(),
        timestamp(expired_at),
        None,
        None,
        AuditFailOn::Never,
    )
    .unwrap();
    let finding = report
        .findings
        .iter()
        .find(|finding| finding.code == "role_coverage_shortfall")
        .unwrap();
    assert_eq!(finding.subject, role_id);
    assert_eq!(finding.evidence["assignments"][0]["disposition"], "expired");
    assert!(finding.evidence["nearest_active_deadline_ts"].is_null());

    let status = actor_status::actor_status(
        &state,
        "alice",
        None,
        timestamp("2030-01-01T00:00:04Z"),
        600,
    );
    assert_eq!(status.work.role_assignments.len(), 1);
    assert_eq!(status.work.role_assignments[0].role_name, "reviewer");
    let expired_status =
        actor_status::actor_status(&state, "alice", None, timestamp(expired_at), 600);
    assert!(expired_status.work.role_assignments.is_empty());

    let first = format!("{:?}", reducer::replay_store(&store).unwrap());
    let second = format!("{:?}", reducer::replay_store(&store).unwrap());
    assert_eq!(first, second);
}

#[test]
fn assignment_deadline_does_not_revive_after_a_later_session_heartbeat() {
    let (_temp, store) = setup();
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "ephemeral",
            "chief",
            1,
            1,
            Vec::new(),
            "2030-01-01T00:00:00Z",
            "define-ephemeral",
        ),
    );
    let session_id = ids::new_session_id();
    let session_op = start_session(&store, "alice", &session_id, "2030-01-01T00:00:01Z", 100);
    let assignment_id = ids::new_role_assignment_id();
    publish_checked(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &assignment_id,
            "chief",
            "alice",
            &session_id,
            &session_op,
            10,
            Vec::new(),
            "2030-01-01T00:00:02Z",
            "assign-ephemeral",
        ),
    );
    publish_checked(
        &store,
        &make_session_heartbeat(
            "alice".into(),
            session_id,
            1_000,
            Some("heartbeat-after-role-expiry".into()),
            timestamp("2030-01-01T00:00:13Z"),
        ),
    );

    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.role_assignment_disposition(
            &state.role_assignments[&assignment_id],
            "2030-01-01T00:00:14Z"
        ),
        RoleAssignmentDisposition::Expired
    );
    assert!(
        state
            .role_active_assignments(&role_id, "2030-01-01T00:00:14Z")
            .is_empty()
    );
}

#[test]
fn protocol_version_and_session_lease_bounds_fail_closed() {
    let (_temp, store) = setup();
    let role_id = ids::new_role_id();
    let mut future_definition = define_role(
        &role_id,
        "future",
        "chief",
        1,
        1,
        Vec::new(),
        "2030-01-01T00:00:00Z",
        "future-version",
    );
    let Op::RoleDefine(definition) = &mut future_definition else {
        unreachable!()
    };
    definition.v = ROLE_PROTOCOL_VERSION + 1;
    publish_rejected(
        &store,
        &future_definition,
        "unsupported role protocol version",
    );

    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "bounded",
            "chief",
            2,
            1,
            Vec::new(),
            "2030-01-01T00:00:01Z",
            "bounded-definition",
        ),
    );
    let session_id = ids::new_session_id();
    let session_op = start_session(&store, "alice", &session_id, "2030-01-01T00:00:02Z", 20);
    publish_rejected(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &ids::new_role_assignment_id(),
            "chief",
            "alice",
            &session_id,
            &session_op,
            21,
            Vec::new(),
            "2030-01-01T00:00:03Z",
            "assignment-outlives-session",
        ),
        "outlive holder session lease",
    );
}

#[test]
fn active_snapshot_and_capacity_make_competing_assignments_fail_closed() {
    let (_temp, store) = setup();
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "release",
            "chief",
            1,
            1,
            Vec::new(),
            "2030-01-01T00:00:00Z",
            "define-release",
        ),
    );
    let alice_session = ids::new_session_id();
    let alice_session_op = start_session(
        &store,
        "alice",
        &alice_session,
        "2030-01-01T00:00:01Z",
        3_600,
    );
    let bob_session = ids::new_session_id();
    let bob_session_op = start_session(&store, "bob", &bob_session, "2030-01-01T00:00:02Z", 3_600);
    let alice_assignment = ids::new_role_assignment_id();
    publish_checked(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &alice_assignment,
            "chief",
            "alice",
            &alice_session,
            &alice_session_op,
            600,
            Vec::new(),
            "2030-01-01T00:00:03Z",
            "capacity-alice",
        ),
    );
    let bob_assignment = ids::new_role_assignment_id();
    publish_rejected(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &bob_assignment,
            "chief",
            "bob",
            &bob_session,
            &bob_session_op,
            600,
            Vec::new(),
            "2030-01-01T00:00:04Z",
            "capacity-stale",
        ),
        "stale active-assignment CAS",
    );
    let state = reducer::replay_store(&store).unwrap();
    publish_rejected(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &bob_assignment,
            "chief",
            "bob",
            &bob_session,
            &bob_session_op,
            600,
            state.role_active_assignment_clocks(&role_id, "2030-01-01T00:00:05Z"),
            "2030-01-01T00:00:05Z",
            "capacity-full",
        ),
        "capacity 1 is full",
    );
}

#[test]
fn renewal_release_and_session_end_are_one_way() {
    let (_temp, store) = setup();
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "deputy",
            "chief",
            1,
            1,
            Vec::new(),
            "2030-01-01T00:00:00Z",
            "define-deputy",
        ),
    );
    let session_id = ids::new_session_id();
    let session_op = start_session(&store, "alice", &session_id, "2030-01-01T00:00:01Z", 1_000);
    let assignment_id = ids::new_role_assignment_id();
    let assignment_op = publish_checked(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &assignment_id,
            "chief",
            "alice",
            &session_id,
            &session_op,
            100,
            Vec::new(),
            "2030-01-01T00:00:02Z",
            "renew-assign",
        ),
    );

    let renew = |actor: &str, expect: &str, ttl_s: u32, key: &str| {
        Op::RoleRenew(RoleRenewOp {
            v: 1,
            op: String::new(),
            ts: "2030-01-01T00:00:03Z".into(),
            actor: actor.into(),
            role_id: role_id.clone(),
            assignment_id: assignment_id.clone(),
            expect_assignment: expect.into(),
            expect_session: session_op.clone(),
            ttl_s,
            idempotency_key: key.into(),
        })
    };
    publish_rejected(
        &store,
        &renew("intruder", &assignment_op, 200, "renew-intruder"),
        "named assignment authority",
    );
    publish_rejected(
        &store,
        &renew("chief", "stale-clock", 200, "renew-stale"),
        "exact current clock",
    );
    publish_rejected(
        &store,
        &renew("chief", &assignment_op, 2_000, "renew-too-long"),
        "outlive holder session lease",
    );
    let renewal_op = publish_checked(
        &store,
        &renew("chief", &assignment_op, 300, "renew-success"),
    );
    let release_op = Op::RoleRelease(RoleReleaseOp {
        v: 1,
        op: String::new(),
        ts: "2030-01-01T00:00:04Z".into(),
        actor: "alice".into(),
        role_id: role_id.clone(),
        assignment_id: assignment_id.clone(),
        expect_assignment: renewal_op.clone(),
        reason: Some("stepping down".into()),
        idempotency_key: "release-holder".into(),
    });
    publish_checked(&store, &release_op);
    publish_rejected(
        &store,
        &Op::RoleRenew(RoleRenewOp {
            v: 1,
            op: String::new(),
            ts: "2030-01-01T00:00:05Z".into(),
            actor: "chief".into(),
            role_id: role_id.clone(),
            assignment_id: assignment_id.clone(),
            expect_assignment: renewal_op,
            expect_session: session_op,
            ttl_s: 300,
            idempotency_key: "renew-after-release".into(),
        }),
        "exact current clock",
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.role_assignment_disposition(
            &state.role_assignments[&assignment_id],
            "2030-01-01T00:00:06Z"
        ),
        RoleAssignmentDisposition::Released
    );
}

#[test]
fn ended_bound_session_does_not_transfer_to_another_live_session() {
    let (_temp, store) = setup();
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "specialist",
            "chief",
            1,
            1,
            Vec::new(),
            "2030-01-01T00:00:00Z",
            "define-specialist",
        ),
    );
    let bound_session = ids::new_session_id();
    let bound_session_op = start_session(
        &store,
        "alice",
        &bound_session,
        "2030-01-01T00:00:01Z",
        1_000,
    );
    let assignment_id = ids::new_role_assignment_id();
    publish_checked(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &assignment_id,
            "chief",
            "alice",
            &bound_session,
            &bound_session_op,
            500,
            Vec::new(),
            "2030-01-01T00:00:02Z",
            "session-end-assign",
        ),
    );
    publish_checked(
        &store,
        &make_session_end(
            "alice".into(),
            bound_session,
            timestamp("2030-01-01T00:00:03Z"),
        ),
    );
    let other_session = ids::new_session_id();
    start_session(
        &store,
        "alice",
        &other_session,
        "2030-01-01T00:00:04Z",
        1_000,
    );
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(
        state.role_assignment_disposition(
            &state.role_assignments[&assignment_id],
            "2030-01-01T00:00:05Z"
        ),
        RoleAssignmentDisposition::SessionEnded
    );
    assert!(
        state
            .active_role_assignments_for_actor("alice", "2030-01-01T00:00:05Z")
            .is_empty()
    );
}

#[test]
fn concurrent_role_exclusion_is_checked_in_both_directions() {
    let (_temp, store) = setup();
    let implementer_role = ids::new_role_id();
    let implementer_definition = publish_checked(
        &store,
        &define_role(
            &implementer_role,
            "implementer",
            "chief",
            2,
            0,
            Vec::new(),
            "2030-01-01T00:00:00Z",
            "define-implementer",
        ),
    );
    let reviewer_role = ids::new_role_id();
    let reviewer_definition = publish_checked(
        &store,
        &define_role(
            &reviewer_role,
            "reviewer",
            "chief",
            2,
            1,
            vec![RoleExclusion {
                code: RoleExclusionCode::ConcurrentRole,
                target_role_id: Some(implementer_role.clone()),
            }],
            "2030-01-01T00:00:01Z",
            "define-reviewer-exclusion",
        ),
    );
    let session_id = ids::new_session_id();
    let session_op = start_session(&store, "alice", &session_id, "2030-01-01T00:00:02Z", 1_000);
    publish_checked(
        &store,
        &assign_role(
            &implementer_role,
            &implementer_definition,
            &ids::new_role_assignment_id(),
            "chief",
            "alice",
            &session_id,
            &session_op,
            500,
            Vec::new(),
            "2030-01-01T00:00:03Z",
            "assign-implementer",
        ),
    );
    publish_rejected(
        &store,
        &assign_role(
            &reviewer_role,
            &reviewer_definition,
            &ids::new_role_assignment_id(),
            "chief",
            "alice",
            &session_id,
            &session_op,
            500,
            Vec::new(),
            "2030-01-01T00:00:04Z",
            "assign-conflicting-reviewer",
        ),
        "conflicts with active role",
    );
}

#[test]
fn retirement_requires_no_active_assignments_and_reserves_identity() {
    let (_temp, store) = setup();
    let role_id = ids::new_role_id();
    let definition = publish_checked(
        &store,
        &define_role(
            &role_id,
            "temporary",
            "chief",
            1,
            0,
            Vec::new(),
            "2030-01-01T00:00:00Z",
            "define-temporary",
        ),
    );
    let session_id = ids::new_session_id();
    let session_op = start_session(&store, "alice", &session_id, "2030-01-01T00:00:01Z", 1_000);
    let assignment_id = ids::new_role_assignment_id();
    let assignment_op = publish_checked(
        &store,
        &assign_role(
            &role_id,
            &definition,
            &assignment_id,
            "chief",
            "alice",
            &session_id,
            &session_op,
            500,
            Vec::new(),
            "2030-01-01T00:00:02Z",
            "retire-assign",
        ),
    );
    let retire = |clocks: Vec<RoleAssignmentClock>, key: &str, at: &str| {
        Op::RoleRetire(RoleRetireOp {
            v: 1,
            op: String::new(),
            ts: at.into(),
            actor: "chief".into(),
            role_id: role_id.clone(),
            expect_definition: definition.clone(),
            expect_assignments: clocks,
            reason: Some("no longer needed".into()),
            idempotency_key: key.into(),
        })
    };
    publish_rejected(
        &store,
        &retire(
            vec![RoleAssignmentClock {
                assignment_id: assignment_id.clone(),
                clock_op_id: assignment_op.clone(),
            }],
            "retire-active",
            "2030-01-01T00:00:03Z",
        ),
        "active assignments",
    );
    let release_op = publish_checked(
        &store,
        &Op::RoleRelease(RoleReleaseOp {
            v: 1,
            op: String::new(),
            ts: "2030-01-01T00:00:04Z".into(),
            actor: "chief".into(),
            role_id: role_id.clone(),
            assignment_id: assignment_id.clone(),
            expect_assignment: assignment_op,
            reason: None,
            idempotency_key: "retire-release".into(),
        }),
    );
    publish_checked(
        &store,
        &retire(
            vec![RoleAssignmentClock {
                assignment_id,
                clock_op_id: release_op,
            }],
            "retire-success",
            "2030-01-01T00:00:05Z",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.roles[&role_id].retired.is_some());
    publish_rejected(
        &store,
        &define_role(
            &ids::new_role_id(),
            "temporary",
            "chief",
            1,
            0,
            Vec::new(),
            "2030-01-01T00:00:06Z",
            "redefine-retired-name",
        ),
        "already reserved",
    );
}

fn run_mote(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn role_cli_json_idempotency_events_and_actor_status_are_consistent() {
    let (temp, store) = setup();
    let alice_session = ids::new_session_id();
    start_session(
        &store,
        "alice",
        &alice_session,
        &ids::format_rfc3339(Timestamp::now()),
        7_200,
    );
    let defined_args = [
        "--json",
        "--actor",
        "chief",
        "role",
        "define",
        "reviewer",
        "--remit",
        "independent review",
        "--assigner",
        "chief",
        "--capacity",
        "2",
        "--minimum-active",
        "1",
        "--exclude",
        "candidate_proposer",
        "--idempotency-key",
        "cli-role-define",
    ];
    let defined = run_mote(temp.path(), &defined_args);
    assert!(
        defined.status.success(),
        "{}",
        String::from_utf8_lossy(&defined.stderr)
    );
    let defined: serde_json::Value = serde_json::from_slice(&defined.stdout).unwrap();
    let role_id = defined["role_id"].as_str().unwrap().to_string();
    assert_eq!(defined["coverage"]["coverage_shortfall"], 1);
    assert_eq!(defined["exclusions"][0]["code"], "candidate_proposer");
    let count_after_define = store.list_op_filenames().unwrap().len();
    assert!(run_mote(temp.path(), &defined_args).status.success());
    assert_eq!(store.list_op_filenames().unwrap().len(), count_after_define);

    let assigned_args = [
        "--json",
        "--actor",
        "chief",
        "role",
        "assign",
        "reviewer",
        "alice",
        "--session",
        &alice_session,
        "--ttl",
        "1h",
        "--idempotency-key",
        "cli-role-assign",
    ];
    let assigned = run_mote(temp.path(), &assigned_args);
    assert!(
        assigned.status.success(),
        "{}",
        String::from_utf8_lossy(&assigned.stderr)
    );
    let assigned: serde_json::Value = serde_json::from_slice(&assigned.stdout).unwrap();
    assert_eq!(assigned["coverage"]["active_count"], 1);
    assert_eq!(assigned["assignments"][0]["disposition"], "active");
    let assignment_id = assigned["assignments"][0]["assignment_id"]
        .as_str()
        .unwrap();
    let count_after_assign = store.list_op_filenames().unwrap().len();
    assert!(run_mote(temp.path(), &assigned_args).status.success());
    assert_eq!(store.list_op_filenames().unwrap().len(), count_after_assign);

    let listed = run_mote(
        temp.path(),
        &["--json", "role", "list", "--holder", "alice"],
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["role_id"], role_id);

    let status = run_mote(temp.path(), &["--json", "actor", "status", "alice"]);
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["work"]["role_assignments"][0]["role_id"], role_id);
    assert_eq!(
        status["work"]["role_assignments"][0]["assignment_id"],
        assignment_id
    );

    let events = run_mote(temp.path(), &["--json", "events", "--kind", "role"]);
    assert!(
        events.status.success(),
        "{}",
        String::from_utf8_lossy(&events.stderr)
    );
    let events: Vec<serde_json::Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(events.iter().any(|event| event["type"] == "role.defined"));
    assert!(events.iter().any(|event| event["type"] == "role.assigned"));
    assert!(events.iter().all(|event| event["category"] == "role"));

    let snapshot = mote::watch::snapshot_value(
        &reducer::replay_store(&store).unwrap(),
        Some("alice"),
        &ids::format_rfc3339(Timestamp::now()),
    );
    assert_eq!(snapshot["roles"][0]["role_id"], role_id);
}

#[test]
fn legacy_store_replays_with_empty_role_maps() {
    let (_temp, store) = setup();
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.roles.is_empty());
    assert!(state.role_names.is_empty());
    assert!(state.role_assignments.is_empty());
}
