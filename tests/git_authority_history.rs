use std::collections::BTreeMap;

use mote::git_authority::history::{Binding, validate_chain};
use mote::git_authority::planner::SyntheticState;
use mote::git_authority::protocol::{Manifest, Request, Transaction, encode, transaction_path};
use mote::git_authority::transport::{GitBlob, GitCommit};
use serde_json::json;

fn blob(bytes: Vec<u8>) -> GitBlob {
    GitBlob {
        mode: "100644".into(),
        oid: "b".repeat(40),
        bytes,
    }
}

fn root() -> GitCommit {
    GitCommit {
        oid: "1".repeat(40),
        parents: vec![],
        files: BTreeMap::from([(
            "manifest.json".into(),
            blob(encode(&Manifest::new("st-test".into())).unwrap()),
        )]),
    }
}

fn request(id: &str, key: &str) -> Request {
    serde_json::from_value(json!({
        "version":1,"request_id":id,"actor":"a","session":"s","timestamp":"2026-10-20T00:00:00Z",
        "command":{"kind":"synthetic.cas","changes":[{"key":key,"expected":null,"value":"value"}]}
    }))
    .unwrap()
}

fn append(commits: &mut Vec<GitCommit>, request: Request) {
    let s = validate_chain(commits, None, None).unwrap();
    let tx = Transaction::prepare(&s.state, &s.head, request).unwrap();
    let mut files = commits.last().unwrap().files.clone();
    files.insert(transaction_path(tx.sequence), blob(encode(&tx).unwrap()));
    commits.push(GitCommit {
        oid: format!("{:040x}", tx.sequence + 1),
        parents: vec![s.head],
        files,
    });
}

#[test]
fn accepted_and_rejected_history_rebuild_identically_with_receipts() {
    let mut commits = vec![root()];
    append(&mut commits, request("z", "task"));
    append(&mut commits, request("a", "task"));
    let a = validate_chain(&commits, None, None).unwrap();
    let b = validate_chain(&commits, None, None).unwrap();
    assert_eq!(a.state, b.state);
    assert_eq!(a.receipts, b.receipts);
    assert_eq!(a.state.sequence, 2);
    assert_eq!(a.state.values["task"].token, "eff-z-000000");
    assert_eq!(
        a.receipts["a"].outcome.status,
        mote::git_authority::protocol::OutcomeStatus::Rejected
    );
}

#[test]
fn binding_and_high_water_mark_detect_foreign_roots_and_rewinds() {
    let mut commits = vec![root()];
    append(&mut commits, request("one", "task"));
    let binding = Binding {
        version: 1,
        endpoint_digest: "digest".into(),
        branch_ref: "refs/heads/mote-state".into(),
        store_id: "st-test".into(),
        generation: 1,
        root: commits[0].oid.clone(),
    };
    validate_chain(&commits, Some(&binding), Some(&commits[1].oid)).unwrap();
    assert!(
        binding
            .check_endpoint("other", "refs/heads/mote-state")
            .is_err()
    );
    let mut wrong = binding.clone();
    wrong.root = "f".repeat(40);
    assert!(validate_chain(&commits, Some(&wrong), None).is_err());
    assert!(validate_chain(&commits[..1], Some(&binding), Some(&commits[1].oid)).is_err());
}

#[test]
fn changed_prefix_merges_unexpected_files_and_nonregular_modes_fail_closed() {
    let mut good = vec![root()];
    append(&mut good, request("one", "task"));
    let mut changed = good.clone();
    changed[1]
        .files
        .get_mut("manifest.json")
        .unwrap()
        .bytes
        .push(b' ');
    assert!(validate_chain(&changed, None, None).is_err());
    let mut merged = good.clone();
    merged[1].parents.push("f".repeat(40));
    assert!(validate_chain(&merged, None, None).is_err());
    let mut extra = good.clone();
    extra[1]
        .files
        .insert("user.txt".into(), blob(b"no".to_vec()));
    assert!(validate_chain(&extra, None, None).is_err());
    let mut executable = good.clone();
    executable[1]
        .files
        .get_mut(&transaction_path(1))
        .unwrap()
        .mode = "100755".into();
    assert!(validate_chain(&executable, None, None).is_err());
    let mut link = good;
    link[0].files.get_mut("manifest.json").unwrap().mode = "120000".into();
    assert!(validate_chain(&link, None, None).is_err());
}

#[test]
fn duplicate_receipt_missing_effect_and_false_rejection_are_invalid_history() {
    let mut good = vec![root()];
    append(&mut good, request("one", "task"));
    let s = validate_chain(&good, None, None).unwrap();
    let mut duplicate = good.clone();
    let tx = Transaction::prepare(&s.state, &s.head, request("one", "another")).unwrap();
    let mut files = good[1].files.clone();
    files.insert(transaction_path(2), blob(encode(&tx).unwrap()));
    duplicate.push(GitCommit {
        oid: "3".repeat(40),
        parents: vec![s.head],
        files,
    });
    assert!(validate_chain(&duplicate, None, None).is_err());
    let mut tx = Transaction::prepare(
        &SyntheticState::default(),
        &good[0].oid,
        request("one", "task"),
    )
    .unwrap();
    tx.effects.clear();
    good[1]
        .files
        .insert(transaction_path(1), blob(encode(&tx).unwrap()));
    assert!(validate_chain(&good, None, None).is_err());
    tx.outcome =
        serde_json::from_value(json!({"status":"rejected","result":{"code":"fabricated"}}))
            .unwrap();
    good[1]
        .files
        .insert(transaction_path(1), blob(encode(&tx).unwrap()));
    assert!(validate_chain(&good, None, None).is_err());
}

#[test]
fn migrated_roots_and_unsupported_profiles_are_refused() {
    let mut imported = root();
    imported
        .files
        .insert("legacy.json".into(), blob(b"{}".to_vec()));
    assert!(validate_chain(&[imported], None, None).is_err());
    let mut unsupported = root();
    let mut manifest = Manifest::new("st-test".into());
    manifest.capabilities.push("unknown".into());
    unsupported
        .files
        .insert("manifest.json".into(), blob(encode(&manifest).unwrap()));
    assert!(validate_chain(&[unsupported], None, None).is_err());
}
