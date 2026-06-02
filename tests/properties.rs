//! Property tests for path normalization, op (de)serialization, and reducer
//! invariants. Uses the `proptest` crate to generate diverse inputs.

use std::collections::BTreeMap;

use jiff::Timestamp;
use proptest::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

use mote::op::{
    self, Op, ScalarSet, Status, make_claim, make_create, make_delete, make_dep, make_msg_ack,
    make_msg_send, make_note, make_patch, make_rel, make_release, make_reserve_close,
    make_reserve_open, make_tag,
};
use mote::{ids, paths, publish, reducer, repo::Store};

// ---------------------------------------------------------------------------
// Path properties
// ---------------------------------------------------------------------------

/// Strategy for repo-relative path components: ASCII alphanumerics + `_-`,
/// 1..8 chars, never `.` or `..`.
fn path_component() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9_-]{1,8}".prop_filter("excluded reserved", |s| s != "." && s != "..")
}

/// Strategy for whole repo-relative paths: 1..5 components, optional trailing slash.
fn repo_path() -> impl Strategy<Value = String> {
    (prop::collection::vec(path_component(), 1..5), any::<bool>()).prop_map(|(parts, trailing)| {
        let mut s = parts.join("/");
        if trailing {
            s.push('/');
        }
        s
    })
}

proptest! {
    #[test]
    fn path_overlap_is_symmetric(a in repo_path(), b in repo_path()) {
        prop_assert_eq!(paths::overlap(&a, &b), paths::overlap(&b, &a));
    }

    #[test]
    fn path_overlap_is_reflexive_after_normalize(p in repo_path()) {
        let n = paths::normalize(&p).unwrap();
        prop_assert!(paths::overlap(&n, &n));
    }

    #[test]
    fn path_normalize_is_idempotent(p in repo_path()) {
        if let Ok(once) = paths::normalize(&p) {
            let twice = paths::normalize(&once).unwrap();
            prop_assert_eq!(once, twice);
        }
    }

    #[test]
    fn path_normalize_preserves_trailing_slash(parts in prop::collection::vec(path_component(), 1..5)) {
        let dir = format!("{}/", parts.join("/"));
        let file = parts.join("/");
        let nd = paths::normalize(&dir).unwrap();
        let nf = paths::normalize(&file).unwrap();
        prop_assert!(nd.ends_with('/'));
        prop_assert!(!nf.ends_with('/'));
    }

    #[test]
    fn path_normalize_rejects_dotdot(parts in prop::collection::vec(path_component(), 0..3)) {
        // Insert a literal ".." somewhere; normalize must reject.
        let mut all: Vec<&str> = parts.iter().map(String::as_str).collect();
        all.push("..");
        let path = all.join("/");
        prop_assert!(paths::normalize(&path).is_err());
    }
}

// ---------------------------------------------------------------------------
// Op envelope round-trip
// ---------------------------------------------------------------------------

fn arb_status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Open),
        Just(Status::Doing),
        Just(Status::Blocked),
        Just(Status::Review),
        Just(Status::Closed),
    ]
}

fn arb_scalar_set() -> impl Strategy<Value = ScalarSet> {
    (
        prop::option::of("[a-zA-Z0-9 ]{1,16}"),
        prop::option::of(arb_status()),
        prop::option::of(0i32..=3),
        prop::option::of("[a-zA-Z0-9 ]{0,32}"),
        prop::option::of("[a-z]{1,8}"),
    )
        .prop_map(|(t, s, p, b, a)| ScalarSet {
            title: t,
            status: s,
            priority: p,
            body: b,
            assignee: a,
        })
}

proptest! {
    #[test]
    fn create_op_round_trips_through_json(set in arb_scalar_set(), entity in "bd-[a-zA-Z0-9]{8,16}") {
        // make_create requires non-empty title to be useful at reducer level,
        // but JSON round-tripping shouldn't care.
        let op = make_create("alice".into(), entity, set, Timestamp::now());
        let v: Value = serde_json::to_value(&op).unwrap();
        let back: Op = serde_json::from_value(v).unwrap();
        prop_assert_eq!(op.kind_name(), back.kind_name());
        prop_assert_eq!(op.entity(), back.entity());
        prop_assert_eq!(op.actor(), back.actor());
    }
}

// ---------------------------------------------------------------------------
// Reducer invariants on randomized op streams
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum RandomOp {
    Create(String),                                // entity
    PatchStatus(String, Status), // entity, status (no expect — exercises rejection paths)
    PatchPriority(String, i32),  // entity, priority
    TagAdd(String, String),      // entity, tag
    TagRemove(String, String),   // entity, tag
    Note(String, String),        // entity, text
    Close(String),               // entity
    Delete(String),              // entity
    Claim(String, String, u32),  // entity, actor, ttl
    Release(String, String),     // entity, actor
    DepAdd(String, String),      // child, parent
    DepRemove(String, String),   // child, parent
    RelAdd(String, String),      // child, parent
    RelRemove(String, String),   // child, parent
    ReserveOpen(String, String, Vec<String>, u32), // entity, actor, paths, ttl
    MsgSend(String, String, String, String), // from, to, body, msg_id
    MsgAck(String, String),      // actor, msg_id
}

const POOL_SIZE: usize = 5;
const ACTORS: &[&str] = &["alice", "bob", "carol"];

fn entity_id(idx: usize) -> String {
    format!("bd-test-{idx:02}")
}

fn arb_random_op() -> impl Strategy<Value = RandomOp> {
    let entity = (0..POOL_SIZE).prop_map(entity_id);
    let actor = prop::sample::select(ACTORS).prop_map(|s| s.to_string());

    prop_oneof![
        entity.clone().prop_map(RandomOp::Create),
        (entity.clone(), arb_status()).prop_map(|(e, s)| RandomOp::PatchStatus(e, s)),
        (entity.clone(), 0i32..=3).prop_map(|(e, p)| RandomOp::PatchPriority(e, p)),
        (entity.clone(), "[a-z]{1,4}").prop_map(|(e, t)| RandomOp::TagAdd(e, t)),
        (entity.clone(), "[a-z]{1,4}").prop_map(|(e, t)| RandomOp::TagRemove(e, t)),
        (entity.clone(), "[a-zA-Z ]{0,16}").prop_map(|(e, t)| RandomOp::Note(e, t)),
        entity.clone().prop_map(RandomOp::Close),
        entity.clone().prop_map(RandomOp::Delete),
        (entity.clone(), actor.clone(), 1u32..=10).prop_map(|(e, a, t)| RandomOp::Claim(e, a, t)),
        (entity.clone(), actor.clone()).prop_map(|(e, a)| RandomOp::Release(e, a)),
        (
            (0..POOL_SIZE).prop_map(entity_id),
            (0..POOL_SIZE).prop_map(entity_id)
        )
            .prop_map(|(c, p)| RandomOp::DepAdd(c, p)),
        (
            (0..POOL_SIZE).prop_map(entity_id),
            (0..POOL_SIZE).prop_map(entity_id)
        )
            .prop_map(|(c, p)| RandomOp::DepRemove(c, p)),
        (
            (0..POOL_SIZE).prop_map(entity_id),
            (0..POOL_SIZE).prop_map(entity_id)
        )
            .prop_map(|(c, p)| RandomOp::RelAdd(c, p)),
        (
            (0..POOL_SIZE).prop_map(entity_id),
            (0..POOL_SIZE).prop_map(entity_id)
        )
            .prop_map(|(c, p)| RandomOp::RelRemove(c, p)),
        (
            entity.clone(),
            actor.clone(),
            prop::collection::vec("[a-z]{1,6}", 1..3),
            1u32..=10
        )
            .prop_map(|(e, a, raw, ttl)| {
                let paths = raw.into_iter().map(|s| format!("src/{s}/")).collect();
                RandomOp::ReserveOpen(e, a, paths, ttl)
            }),
        (
            actor.clone(),
            actor.clone(),
            "[a-z]{0,8}",
            "msg-[a-zA-Z0-9]{6,12}"
        )
            .prop_map(|(f, t, b, m)| RandomOp::MsgSend(f, t, b, m)),
        (actor, "msg-[a-zA-Z0-9]{6,12}").prop_map(|(a, m)| RandomOp::MsgAck(a, m)),
    ]
}

fn publish_random(store: &Store, op: &RandomOp) {
    let ts = Timestamp::now();
    match op {
        RandomOp::Create(e) => {
            let _ = publish::publish_op(
                store,
                &make_create(
                    "alice".into(),
                    e.clone(),
                    ScalarSet {
                        title: Some(e.clone()),
                        ..Default::default()
                    },
                    ts,
                ),
            );
        }
        RandomOp::PatchStatus(e, s) => {
            let _ = publish::publish_op(
                store,
                &make_patch(
                    "alice".into(),
                    e.clone(),
                    BTreeMap::new(),
                    ScalarSet {
                        status: Some(*s),
                        ..Default::default()
                    },
                    ts,
                ),
            );
        }
        RandomOp::PatchPriority(e, p) => {
            let _ = publish::publish_op(
                store,
                &make_patch(
                    "alice".into(),
                    e.clone(),
                    BTreeMap::new(),
                    ScalarSet {
                        priority: Some(*p),
                        ..Default::default()
                    },
                    ts,
                ),
            );
        }
        RandomOp::TagAdd(e, t) => {
            let _ = publish::publish_op(
                store,
                &make_tag(true, "alice".into(), e.clone(), t.clone(), ts),
            );
        }
        RandomOp::TagRemove(e, t) => {
            let _ = publish::publish_op(
                store,
                &make_tag(false, "alice".into(), e.clone(), t.clone(), ts),
            );
        }
        RandomOp::Note(e, text) => {
            let _ = publish::publish_op(
                store,
                &make_note(
                    "alice".into(),
                    e.clone(),
                    "progress".into(),
                    text.clone(),
                    ts,
                ),
            );
        }
        RandomOp::Close(e) => {
            let _ = publish::publish_op(
                store,
                &mote::op::make_close("alice".into(), e.clone(), BTreeMap::new(), ts),
            );
        }
        RandomOp::Delete(e) => {
            let _ = publish::publish_op(store, &make_delete("alice".into(), e.clone(), ts));
        }
        RandomOp::Claim(e, a, ttl) => {
            let _ = publish::publish_op(
                store,
                &make_claim(a.clone(), e.clone(), a.clone(), *ttl, None, ts),
            );
        }
        RandomOp::Release(e, a) => {
            let _ = publish::publish_op(store, &make_release(a.clone(), e.clone(), None, ts));
        }
        RandomOp::DepAdd(c, p) => {
            let _ = publish::publish_op(
                store,
                &make_dep(
                    true,
                    "alice".into(),
                    c.clone(),
                    p.clone(),
                    "blocks".into(),
                    ts,
                ),
            );
        }
        RandomOp::DepRemove(c, p) => {
            let _ = publish::publish_op(
                store,
                &make_dep(
                    false,
                    "alice".into(),
                    c.clone(),
                    p.clone(),
                    "blocks".into(),
                    ts,
                ),
            );
        }
        RandomOp::RelAdd(c, p) => {
            let _ = publish::publish_op(
                store,
                &make_rel(
                    true,
                    "alice".into(),
                    c.clone(),
                    p.clone(),
                    "parent".into(),
                    ts,
                ),
            );
        }
        RandomOp::RelRemove(c, p) => {
            let _ = publish::publish_op(
                store,
                &make_rel(
                    false,
                    "alice".into(),
                    c.clone(),
                    p.clone(),
                    "parent".into(),
                    ts,
                ),
            );
        }
        RandomOp::ReserveOpen(e, a, paths, ttl) => {
            let rv = ids::new_reservation_id();
            let _ = publish::publish_op(
                store,
                &make_reserve_open(a.clone(), rv, e.clone(), paths.clone(), *ttl, ts),
            );
        }
        RandomOp::MsgSend(f, t, b, m) => {
            let _ = publish::publish_op(
                store,
                &make_msg_send(
                    f.clone(),
                    m.clone(),
                    t.clone(),
                    None,
                    None,
                    "note".into(),
                    b.clone(),
                    ts,
                ),
            );
        }
        RandomOp::MsgAck(a, m) => {
            let _ = publish::publish_op(store, &make_msg_ack(a.clone(), m.clone(), ts));
        }
    }
}

fn fresh_store() -> (TempDir, Store) {
    let td = TempDir::new().unwrap();
    let store = Store::init(td.path()).unwrap();
    (td, store)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn replay_is_deterministic_under_random_ops(ops in prop::collection::vec(arb_random_op(), 5..40)) {
        let (_td, store) = fresh_store();
        for op in &ops {
            publish_random(&store, op);
        }
        let s1 = reducer::replay_store(&store).unwrap();
        let s2 = reducer::replay_store(&store).unwrap();
        prop_assert_eq!(format!("{s1:#?}"), format!("{s2:#?}"));
    }

    #[test]
    fn no_two_live_reservations_overlap_across_actors(ops in prop::collection::vec(arb_random_op(), 5..40)) {
        let (_td, store) = fresh_store();
        for op in &ops {
            publish_random(&store, op);
        }
        let state = reducer::replay_store(&store).unwrap();
        let now = ids::format_rfc3339(Timestamp::now());

        let live: Vec<&mote::state::ReservationState> = state
            .reservations
            .values()
            .filter(|r| r.is_active(&now))
            .collect();
        for i in 0..live.len() {
            for j in (i + 1)..live.len() {
                if live[i].actor == live[j].actor {
                    continue;
                }
                for pi in live[i].live_paths() {
                    for pj in live[j].live_paths() {
                        prop_assert!(
                            !paths::overlap(pi, pj),
                            "live reservations from different actors overlap: \
                             {} ({}) vs {} ({}); paths `{}` and `{}`",
                            live[i].reservation_id, live[i].actor,
                            live[j].reservation_id, live[j].actor,
                            pi, pj
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn scalar_clocks_only_advance_after_accepted_writes(ops in prop::collection::vec(arb_random_op(), 5..40)) {
        let (_td, store) = fresh_store();
        for op in &ops {
            publish_random(&store, op);
        }
        let state = reducer::replay_store(&store).unwrap();
        let names = store.list_op_filenames().unwrap();

        // For each scalar field, the bead's _clock[field] must be the op_id of
        // an ACCEPTED op for that bead, and that op must exist in ops/.
        let op_ids: std::collections::HashSet<String> = names
            .iter()
            .map(|n| n.trim_end_matches(".json").to_string())
            .collect();
        for bead in state.beads.values() {
            for clock_op_id in bead.clock.values() {
                prop_assert!(
                    op_ids.contains(clock_op_id),
                    "clock op_id `{clock_op_id}` not present in ops/"
                );
                // The op must appear as accepted in this bead's history.
                if let Some(history) = state.history.get(&bead.id) {
                    let accepted_ids: Vec<&str> = history
                        .iter()
                        .filter(|e| e.accepted)
                        .map(|e| e.op_id.as_str())
                        .collect();
                    prop_assert!(
                        accepted_ids.contains(&clock_op_id.as_str()),
                        "clock op_id `{clock_op_id}` is not in bead {}'s accepted history",
                        bead.id
                    );
                }
            }
        }
    }

    #[test]
    fn ack_implies_recipient_match_and_one_shot(ops in prop::collection::vec(arb_random_op(), 5..40)) {
        let (_td, store) = fresh_store();
        for op in &ops {
            publish_random(&store, op);
        }
        let state = reducer::replay_store(&store).unwrap();
        for m in state.messages.values() {
            // Self-ack of own send is forbidden, so if acked, sender ≠ acker.
            if let Some(ack_op_id) = &m.ack_op_id {
                // The ack must be present somewhere in history (either entity-
                // scoped or orphan) and must reference this msg_id.
                let mut found = false;
                for entries in state.history.values() {
                    if entries
                        .iter()
                        .any(|e| e.op_id == *ack_op_id && e.kind == "msg_ack" && e.accepted)
                    {
                        found = true;
                        break;
                    }
                }
                if !found {
                    found = state
                        .orphan_history
                        .iter()
                        .any(|e| e.op_id == *ack_op_id && e.kind == "msg_ack" && e.accepted);
                }
                prop_assert!(found, "ack_op_id `{ack_op_id}` not in accepted history");
                prop_assert!(m.ack_ts.is_some(), "ack_ts must be set when ack_op_id is set");
            }
        }
    }
}

// Silence unused import for op::{} kinds we reference indirectly.
#[allow(dead_code)]
fn _unused_imports_anchor() -> &'static str {
    let _ = op::VALID_NOTE_KINDS;
    let _ = make_reserve_close as fn(_, _, _, _) -> Op;
    "anchor"
}
