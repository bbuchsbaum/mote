use std::collections::BTreeMap;

use mote::git_authority::planner::{Cell, SyntheticState, plan};
use mote::git_authority::protocol::{Manifest, Outcome, Request, Transaction, decode, encode};
use serde_json::{Value, json};

fn fixtures() -> Value {
    serde_json::from_str(include_str!("fixtures/git_authority/contracts.json")).unwrap()
}

#[test]
fn manifests_and_requests_follow_the_reviewed_contract_vectors() {
    let f = fixtures();
    let base = f["valid_manifests"][0].clone();
    decode::<Manifest>(&serde_json::to_vec(&base).unwrap())
        .unwrap()
        .validate()
        .unwrap();
    for mutation in f["invalid_manifest_mutations"].as_array().unwrap() {
        let mut input = base.clone();
        input[mutation["field"].as_str().unwrap()] = mutation["value"].clone();
        let result = decode::<Manifest>(&serde_json::to_vec(&input).unwrap())
            .and_then(|manifest| manifest.validate());
        assert!(result.is_err(), "manifest mutation should fail: {mutation}");
    }
    let base = f["valid_requests"][0].clone();
    decode::<Request>(&serde_json::to_vec(&base).unwrap())
        .unwrap()
        .validate()
        .unwrap();
    for mutation in f["invalid_request_mutations"].as_array().unwrap() {
        let mut input = base.clone();
        input[mutation["field"].as_str().unwrap()] = mutation["value"].clone();
        let result = decode::<Request>(&serde_json::to_vec(&input).unwrap())
            .and_then(|request| request.validate());
        assert!(result.is_err(), "request mutation should fail: {mutation}");
    }
}

#[test]
fn transition_vectors_match_effects_results_and_final_state() {
    for vector in fixtures()["vectors"].as_array().unwrap() {
        let request: Request = decode(&serde_json::to_vec(&vector["request"]).unwrap()).unwrap();
        let state = SyntheticState {
            sequence: vector["sequence"].as_u64().unwrap() - 1,
            values: serde_json::from_value(vector["parent_state"].clone()).unwrap(),
            ..Default::default()
        };
        let planned = plan(&state, &request).unwrap();
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
        assert_eq!(
            serde_json::to_value(&planned.next_state.values).unwrap(),
            vector["result_state"],
            "{}",
            vector["name"]
        );
        assert_eq!(planned.next_state.sequence, state.sequence + 1);
        assert_eq!(
            plan(&state, &request).unwrap(),
            planned,
            "planning must be pure"
        );
    }
}

#[test]
fn complete_transaction_and_independent_digest_vector_match() {
    let expected = fixtures()["complete_transaction"].clone();
    let transaction: Transaction = decode(&serde_json::to_vec(&expected).unwrap()).unwrap();
    let prepared = Transaction::prepare(
        &SyntheticState::default(),
        &"1".repeat(40),
        transaction.request.clone(),
    )
    .unwrap();
    assert_eq!(transaction, prepared);
    assert_eq!(
        transaction.request_digest,
        "103dc087ee7bdfdc6c19f6694651157efec9f6a9ae9eb3beaf1a27df5f335cc9"
    );
    transaction
        .validate(&SyntheticState::default(), &"1".repeat(40))
        .unwrap();
    assert_eq!(
        decode::<Transaction>(&encode(&transaction).unwrap()).unwrap(),
        transaction
    );
}

#[test]
fn semantic_validator_refuses_false_rejection_missing_effect_and_changed_intent() {
    let state = SyntheticState::default();
    let expected = fixtures()["complete_transaction"].clone();
    let transaction: Transaction = decode(&serde_json::to_vec(&expected).unwrap()).unwrap();
    let mut missing = transaction.clone();
    missing.effects.clear();
    assert!(missing.validate(&state, &transaction.parent).is_err());
    let mut false_rejection = missing;
    false_rejection.outcome = serde_json::from_value::<Outcome>(json!({
        "status":"rejected", "result":{"code":"compare_failed","index":0,
        "key":"task-1","expected":null,"actual":"invented"}
    }))
    .unwrap();
    assert!(
        false_rejection
            .validate(&state, &transaction.parent)
            .is_err()
    );
    let mut changed = transaction.clone();
    changed.request.actor = "another-actor".into();
    assert!(changed.validate(&state, &transaction.parent).is_err());
    let mut wrong_position = transaction.clone();
    match &mut wrong_position.effects[0] {
        mote::git_authority::protocol::Effect::SyntheticSet { sequence, .. } => *sequence = 2,
        _ => panic!("synthetic fixture must retain its effect type"),
    }
    assert!(
        wrong_position
            .validate(&state, &transaction.parent)
            .is_err()
    );
}

#[test]
fn duplicate_keys_float_syntax_unknown_commands_and_missing_null_fields_fail() {
    for bytes in [
        br#"{"a":1,"a":2}"#.as_slice(),
        br#"{"nested":{"a":1,"a":2}}"#,
        br#"{"a":1.0}"#,
        br#"{"a":1e0}"#,
    ] {
        assert!(decode::<Value>(bytes).is_err());
    }
    let request = fixtures()["valid_requests"][0].clone();
    for field in ["expected", "value"] {
        let mut input = request.clone();
        input["command"]["changes"][0]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            decode::<Request>(&serde_json::to_vec(&input).unwrap()).is_err(),
            "missing {field}"
        );
    }
    let mut input = request.clone();
    input["command"]["kind"] = json!("task.begin");
    assert!(decode::<Request>(&serde_json::to_vec(&input).unwrap()).is_err());
    let mut input = request;
    let duplicate = input["command"]["changes"][0].clone();
    input["command"]["changes"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(
        decode::<Request>(&serde_json::to_vec(&input).unwrap())
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn effect_identity_does_not_choose_chronological_order() {
    let f = fixtures();
    let vectors = f["position_vectors"].as_array().unwrap();
    let mut state = SyntheticState::default();
    let mut effects = Vec::new();
    for v in vectors {
        let request: Request = serde_json::from_value(json!({
            "version":1,"request_id":v["request_id"],"actor":"a","session":"s",
            "timestamp":v["timestamp"],"command":{"kind":"synthetic.cas","changes":[{
                "key":v["request_id"],"expected":null,"value":"v"
            }]}
        }))
        .unwrap();
        let p = plan(&state, &request).unwrap();
        assert_eq!(p.effects[0].effect_id(), v["effect_id"].as_str().unwrap());
        assert_eq!(p.effects[0].adapter_id(), v["adapter_id"]);
        effects.push(p.effects[0].clone());
        state = p.next_state;
    }
    assert!(effects[0].effect_id() > effects[1].effect_id());
    assert!(effects[0].adapter_id() < effects[1].adapter_id());
}

#[test]
fn sequence_overflow_and_invalid_parent_fail_without_effects() {
    let request: Request = serde_json::from_value(fixtures()["valid_requests"][0].clone()).unwrap();
    let state = SyntheticState {
        sequence: u64::MAX,
        values: BTreeMap::<String, Cell>::new(),
        ..Default::default()
    };
    assert!(plan(&state, &request).is_err());
    assert!(Transaction::prepare(&SyntheticState::default(), "bad", request).is_err());
}
