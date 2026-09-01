use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};

use jiff::{SignedDuration, Timestamp};
use mote::audit::{AuditFailOn, GitBackingMode, inspect_git_backing, run_audit};
use mote::candidate::{
    CandidateEvidencePayload, CandidateSnapshotProvenance, EvidenceOutcome, EvidenceRequirement,
    GIT_ANCESTRY_EVIDENCE, GIT_RELATION_SCHEMA_V2, GitAncestryReceipt,
};
use mote::ids;
use mote::op::{
    CandidateEvidenceOp, CandidateProposeOp, Op, ScalarSet, make_create, make_msg_send,
    make_session_start,
};
use mote::{Store, publish, reducer};
use tempfile::TempDir;

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_git(dir: &Path) {
    run_git(dir, &["init", "-q"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    run_git(dir, &["config", "user.name", "Test"]);
    fs::write(dir.join("work.txt"), "base\n").unwrap();
    run_git(dir, &["add", "work.txt"]);
    run_git(dir, &["commit", "-qm", "base"]);
}

fn run_mote(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mote"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn run_mote_ok(dir: &Path, actor: &str, args: &[&str]) -> String {
    let mut all = vec!["--actor", actor];
    all.extend_from_slice(args);
    let output = run_mote(dir, &all);
    assert!(
        output.status.success(),
        "mote {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn publish_checked(store: &Store, op: &Op) -> String {
    let name = publish::publish_op(store, op).unwrap();
    let state = reducer::replay_store(store).unwrap();
    assert!(
        state.was_accepted(name.as_str()),
        "rejected: {:?}",
        state.rejection_reason(name.as_str())
    );
    name.into_string()
}

fn op_bytes(store: &Store) -> BTreeMap<String, Vec<u8>> {
    store
        .list_op_filenames()
        .unwrap()
        .into_iter()
        .map(|name| {
            let bytes = fs::read(store.ops_dir().join(&name)).unwrap();
            (name, bytes)
        })
        .collect()
}

#[test]
fn git_backing_distinguishes_non_git_local_clean_and_ignored_dirty_stores() {
    let outside = TempDir::new().unwrap();
    let outside_store = Store::init(outside.path()).unwrap();
    assert_eq!(
        inspect_git_backing(&outside_store, Timestamp::now()).mode,
        GitBackingMode::NotInGit
    );

    let temp = TempDir::new().unwrap();
    init_git(temp.path());
    let store = Store::init(temp.path()).unwrap();
    let issue = run_mote_ok(temp.path(), "alice", &["new", "tracked baseline"]);
    let local = inspect_git_backing(&store, Timestamp::now());
    assert_eq!(local.mode, GitBackingMode::LocalUntracked);
    assert_eq!(local.uncommitted_op_count, 1);
    assert!(local.warning().is_none());

    run_git(temp.path(), &["add", ".mote/ops"]);
    run_git(temp.path(), &["commit", "-qm", "track mote baseline"]);
    let clean = inspect_git_backing(&store, Timestamp::now());
    assert_eq!(clean.mode, GitBackingMode::GitBacked);
    assert_eq!(clean.tracked_op_count, 1);
    assert_eq!(clean.uncommitted_op_count, 0);

    fs::write(temp.path().join(".gitignore"), ".mote/ops/*.json\n").unwrap();
    run_git(temp.path(), &["add", ".gitignore"]);
    run_git(temp.path(), &["commit", "-qm", "ignore later mote ops"]);
    run_mote_ok(
        temp.path(),
        "alice",
        &["note", &issue, "--kind", "progress", "later"],
    );
    let future = Timestamp::now()
        .checked_add(SignedDuration::from_secs(3_600))
        .unwrap();
    let dirty = inspect_git_backing(&store, future);
    assert_eq!(dirty.mode, GitBackingMode::GitBacked);
    assert_eq!(
        dirty.untracked_op_count, 1,
        "ignored ops must still be visible"
    );
    assert_eq!(dirty.uncommitted_op_count, 1);
    assert!(dirty.oldest_uncommitted_op_ts.is_some());
    assert!(dirty.oldest_uncommitted_age_s.unwrap() >= 3_599);
    assert!(
        dirty
            .warning()
            .unwrap()
            .contains("clones populated from Git")
    );
}

#[test]
fn git_backing_counts_modified_staged_deleted_and_detached_states() {
    let temp = TempDir::new().unwrap();
    init_git(temp.path());
    let store = Store::init(temp.path()).unwrap();
    for title in ["first", "second"] {
        run_mote_ok(temp.path(), "alice", &["new", title]);
    }
    run_git(temp.path(), &["add", ".mote/ops"]);
    run_git(temp.path(), &["commit", "-qm", "track two ops"]);
    let baseline = store.list_op_filenames().unwrap();

    fs::OpenOptions::new()
        .append(true)
        .open(store.ops_dir().join(&baseline[0]))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    fs::remove_file(store.ops_dir().join(&baseline[1])).unwrap();
    run_mote_ok(temp.path(), "alice", &["new", "staged"]);
    let staged = store
        .list_op_filenames()
        .unwrap()
        .into_iter()
        .find(|name| !baseline.contains(name))
        .unwrap();
    run_git(temp.path(), &["add", &format!(".mote/ops/{staged}")]);
    run_git(temp.path(), &["checkout", "--detach", "-q"]);

    let report = inspect_git_backing(&store, Timestamp::now());
    assert_eq!(report.mode, GitBackingMode::GitBacked);
    assert_eq!(report.modified_op_count, 1);
    assert_eq!(report.deleted_op_count, 1);
    assert_eq!(report.staged_op_count, 1);
    assert_eq!(report.uncommitted_op_count, 3);
    assert_eq!(report.head.as_deref().map(str::len), Some(40));
}

#[test]
fn git_backing_counts_malformed_uncommitted_op_names_without_inventing_an_age() {
    let temp = TempDir::new().unwrap();
    init_git(temp.path());
    let store = Store::init(temp.path()).unwrap();
    run_mote_ok(temp.path(), "alice", &["new", "baseline"]);
    run_git(temp.path(), &["add", ".mote/ops"]);
    run_git(temp.path(), &["commit", "-qm", "track baseline"]);

    fs::write(store.ops_dir().join("not-an-op.json"), "{}\n").unwrap();
    let report = inspect_git_backing(&store, Timestamp::now());

    assert_eq!(report.mode, GitBackingMode::GitBacked);
    assert_eq!(report.uncommitted_op_count, 1);
    assert_eq!(report.untracked_op_count, 1);
    assert_eq!(report.unparseable_uncommitted_count, 1);
    assert_eq!(report.oldest_uncommitted_op_ts, None);
    assert_eq!(report.oldest_uncommitted_age_s, None);
    assert!(
        report
            .detail
            .unwrap()
            .contains("no parseable operation timestamp")
    );
}

#[test]
fn doctor_reports_git_visibility_without_turning_a_warning_into_corruption() {
    let temp = TempDir::new().unwrap();
    init_git(temp.path());
    let store = Store::init(temp.path()).unwrap();
    run_mote_ok(temp.path(), "alice", &["new", "baseline"]);
    run_git(temp.path(), &["add", ".mote/ops"]);
    run_git(temp.path(), &["commit", "-qm", "track baseline"]);
    run_mote_ok(temp.path(), "alice", &["new", "not synchronized"]);
    let before = op_bytes(&store);

    let output = run_mote(temp.path(), &["--json", "--actor", "alice", "doctor"]);
    assert_eq!(output.status.code(), Some(0));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["git_backing"]["mode"], "git_backed");
    assert_eq!(value["git_backing"]["uncommitted_op_count"], 1);
    assert!(value["warnings"].as_array().unwrap().iter().any(|warning| {
        warning
            .as_str()
            .unwrap()
            .contains("clones populated from Git cannot observe")
    }));

    let human = run_mote(temp.path(), &["--actor", "alice", "doctor"]);
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("git:    git_backed"), "{human}");
    assert!(human.contains("uncommitted operation"), "{human}");
    assert_eq!(op_bytes(&store), before);
}

#[test]
fn audit_fail_thresholds_are_read_only_and_json_is_stable() {
    let temp = TempDir::new().unwrap();
    init_git(temp.path());
    let store = Store::init(temp.path()).unwrap();
    run_mote_ok(temp.path(), "alice", &["new", "baseline"]);
    run_git(temp.path(), &["add", ".mote/ops"]);
    run_git(temp.path(), &["commit", "-qm", "track baseline"]);
    run_mote_ok(temp.path(), "alice", &["new", "dirty"]);
    let before = op_bytes(&store);

    let default = run_mote(temp.path(), &["--json", "audit"]);
    assert_eq!(
        default.status.code(),
        Some(0),
        "warnings do not fail default"
    );
    let default: serde_json::Value = serde_json::from_slice(&default.stdout).unwrap();
    assert_eq!(default["schema_version"], 1);
    assert_eq!(default["ok"], true);
    assert_eq!(default["fail_on"], "error");
    assert_eq!(default["summary"]["warning"], 1);

    let strict = run_mote(temp.path(), &["--json", "audit", "--fail-on", "warning"]);
    assert_eq!(strict.status.code(), Some(2));
    let strict: serde_json::Value = serde_json::from_slice(&strict.stdout).unwrap();
    assert_eq!(strict["ok"], false);
    assert_eq!(strict["findings"][0]["code"], "git_store_uncommitted_ops");
    assert_eq!(op_bytes(&store), before);

    let invalid = run_mote(temp.path(), &["audit", "--fail-on", "sometimes"]);
    assert_eq!(invalid.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&invalid.stderr)
            .contains("--fail-on must be error | warning | never")
    );
    assert_eq!(op_bytes(&store), before);
}

#[allow(clippy::too_many_arguments)]
fn candidate_proposal(
    store: &Store,
    candidate_id: &str,
    issue: &str,
    repository_id: &str,
    commit_oid: &str,
    base_oid: &str,
    parent_oids: Vec<String>,
    key: &str,
) -> Op {
    Op::CandidatePropose(CandidateProposeOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        entity: issue.into(),
        store_id: store.read_format().unwrap().store_id,
        repository_id: repository_id.into(),
        landing_repository_id: None,
        object_source: None,
        object_format: "sha1".into(),
        commit_oid: commit_oid.into(),
        base_oid: base_oid.into(),
        parent_oids,
        paths: vec!["work.txt".into()],
        authorizer: "authorizer".into(),
        reviewers: vec!["reviewer".into()],
        review_policy: None,
        evidence_requirements: vec![EvidenceRequirement {
            name: GIT_ANCESTRY_EVIDENCE.into(),
            kind: "git".into(),
            producers: vec!["proposer".into()],
        }],
        evidence_refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

fn ancestry_evidence(
    candidate_id: &str,
    commit_oid: &str,
    base_oid: &str,
    parent_oids: Vec<String>,
    repository_id: &str,
    snapshot: Option<CandidateSnapshotProvenance>,
    key: &str,
) -> Op {
    let payload = CandidateEvidencePayload::GitAncestry(GitAncestryReceipt {
        relation_schema: if snapshot.is_some() {
            GIT_RELATION_SCHEMA_V2
        } else {
            1
        },
        repository_id: repository_id.into(),
        object_format: "sha1".into(),
        common_dir_hash: "test".into(),
        commit_oid: commit_oid.into(),
        base_oid: base_oid.into(),
        parent_oids,
        base_is_ancestor: Some(true),
        candidate_relations: Vec::new(),
        covered_candidates: Vec::new(),
        producer_snapshot: snapshot.map(Box::new),
        git_version: "git version test".into(),
        detail: None,
    });
    Op::CandidateEvidence(CandidateEvidenceOp {
        v: 1,
        op: String::new(),
        ts: ids::format_rfc3339(Timestamp::now()),
        actor: "proposer".into(),
        candidate_id: candidate_id.into(),
        candidate_oid: commit_oid.into(),
        evidence_id: mote::candidate::evidence_id(&payload).unwrap(),
        name: GIT_ANCESTRY_EVIDENCE.into(),
        evidence_kind: "git".into(),
        producer_tool: "git version test".into(),
        outcome: EvidenceOutcome::Pass,
        payload,
        refs: Vec::new(),
        idempotency_key: key.into(),
    })
}

#[test]
fn audit_links_a_snapshot_gap_to_the_uncommitted_target_proposal() {
    let temp = TempDir::new().unwrap();
    init_git(temp.path());
    fs::write(temp.path().join("work.txt"), "candidate\n").unwrap();
    run_git(temp.path(), &["commit", "-qam", "candidate"]);
    let commit = run_git(temp.path(), &["rev-parse", "HEAD"]);
    let base = run_git(temp.path(), &["rev-parse", "HEAD^"]);
    let identity = mote::candidate::probe_ancestry(temp.path(), &commit, &base, &[]).unwrap();
    let store = Store::init(temp.path()).unwrap();
    let issue = ids::new_bead_id();
    publish_checked(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("snapshot audit".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    );
    let first = ids::new_candidate_id();
    publish_checked(
        &store,
        &candidate_proposal(
            &store,
            &first,
            &issue,
            &identity.repository_id,
            &commit,
            &base,
            vec![base.clone()],
            "first-proposal",
        ),
    );
    publish_checked(
        &store,
        &ancestry_evidence(
            &first,
            &commit,
            &base,
            vec![base.clone()],
            &identity.repository_id,
            None,
            "first-evidence",
        ),
    );
    run_git(temp.path(), &["add", ".mote/ops"]);
    run_git(
        temp.path(),
        &["commit", "-qm", "tracked candidate baseline"],
    );

    let second = ids::new_candidate_id();
    let second_proposal = publish_checked(
        &store,
        &candidate_proposal(
            &store,
            &second,
            &issue,
            &identity.repository_id,
            &commit,
            &base,
            vec![base.clone()],
            "second-proposal",
        ),
    );
    publish_checked(
        &store,
        &ancestry_evidence(
            &first,
            &commit,
            &base,
            vec![base.clone()],
            &identity.repository_id,
            Some(CandidateSnapshotProvenance {
                store_id: store.read_format().unwrap().store_id,
                observed_candidates: Vec::new(),
                replayed_op_count: 3,
                replayed_op_ids_digest: "producer-clone-without-second".into(),
                store_git_head: Some(commit.clone()),
                uncommitted_op_count: Some(0),
            }),
            "first-incomplete-refresh",
        ),
    );
    let before = store.list_op_filenames().unwrap();

    let state = reducer::replay_store(&store).unwrap();
    let without_target = run_audit(
        &store,
        &state,
        temp.path(),
        Timestamp::now(),
        None,
        None,
        AuditFailOn::Never,
    )
    .unwrap();
    assert_eq!(without_target.summary.skipped, 2);
    assert!(
        without_target
            .findings
            .iter()
            .all(|finding| finding.code != "candidate_reachable_unrecorded")
    );

    let against_base = run_audit(
        &store,
        &state,
        temp.path(),
        Timestamp::now(),
        Some(&base),
        None,
        AuditFailOn::Never,
    )
    .unwrap();
    assert!(
        against_base
            .findings
            .iter()
            .all(|finding| finding.code != "candidate_reachable_unrecorded")
    );

    let missing_ref = run_audit(
        &store,
        &state,
        temp.path(),
        Timestamp::now(),
        Some("refs/heads/does-not-exist"),
        None,
        AuditFailOn::Never,
    )
    .unwrap();
    assert!(
        missing_ref
            .findings
            .iter()
            .any(|finding| finding.code == "candidate_git_unavailable")
    );

    let output = run_mote(temp.path(), &["--json", "audit", "--target-ref", "HEAD"]);
    assert_eq!(output.status.code(), Some(2));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let gap = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["code"] == "candidate_pair_snapshot_gap")
        .unwrap();
    assert_eq!(gap["subject"], first);
    assert_eq!(gap["evidence"]["missing_proposal_op_id"], second_proposal);
    assert_eq!(gap["evidence"]["uncommitted_in_target"], true);
    assert!(
        gap["message"]
            .as_str()
            .unwrap()
            .contains("cannot repair the target")
    );
    assert!(value["findings"].as_array().unwrap().iter().any(|finding| {
        finding["code"] == "candidate_reachable_unrecorded" && finding["subject"] == first
    }));
    assert_eq!(store.list_op_filenames().unwrap(), before);
    let state = reducer::replay_store(&store).unwrap();
    assert_eq!(state.candidates[&first].phase.as_str(), "pending");
}

#[test]
fn audit_target_reachability_skips_other_repositories_and_reports_shallow_objects() {
    let source = TempDir::new().unwrap();
    init_git(source.path());
    let shallow_only_missing = run_git(source.path(), &["rev-parse", "HEAD"]);
    fs::write(source.path().join("work.txt"), "tip\n").unwrap();
    run_git(source.path(), &["commit", "-qam", "tip"]);

    let clone_parent = TempDir::new().unwrap();
    let clone = clone_parent.path().join("shallow");
    let source_url = format!("file://{}", source.path().display());
    run_git(
        clone_parent.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &source_url,
            clone.to_str().unwrap(),
        ],
    );
    assert!(clone.join(".git/shallow").is_file());

    let store = Store::init(&clone).unwrap();
    let issue = ids::new_bead_id();
    publish_checked(
        &store,
        &make_create(
            "proposer".into(),
            issue.clone(),
            ScalarSet {
                title: Some("shallow reachability".into()),
                ..Default::default()
            },
            Timestamp::now(),
        ),
    );
    let tip = run_git(&clone, &["rev-parse", "HEAD"]);
    let identity = mote::candidate::probe_ancestry(&clone, &tip, &tip, &[]).unwrap();

    let shallow_candidate = ids::new_candidate_id();
    publish_checked(
        &store,
        &candidate_proposal(
            &store,
            &shallow_candidate,
            &issue,
            &identity.repository_id,
            &shallow_only_missing,
            &shallow_only_missing,
            Vec::new(),
            "shallow-proposal",
        ),
    );
    let other_repository = ids::new_candidate_id();
    publish_checked(
        &store,
        &candidate_proposal(
            &store,
            &other_repository,
            &issue,
            "repo-different-object-database",
            &tip,
            &tip,
            Vec::new(),
            "other-repository-proposal",
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    let before = op_bytes(&store);

    let report = run_audit(
        &store,
        &state,
        &clone,
        Timestamp::now(),
        Some("HEAD"),
        None,
        AuditFailOn::Never,
    )
    .unwrap();

    assert_eq!(report.summary.skipped, 1);
    assert_eq!(report.context.target_oid.as_deref(), Some(tip.as_str()));
    assert!(report.findings.iter().any(|finding| {
        finding.code == "candidate_object_unreachable" && finding.subject == shallow_candidate
    }));
    assert!(report.findings.iter().all(|finding| {
        finding.code != "candidate_reachable_unrecorded" || finding.subject != other_repository
    }));
    assert_eq!(op_bytes(&store), before);
}

#[test]
fn audit_surfaces_graph_and_lease_facts_without_inferring_mutations() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let parent = run_mote_ok(temp.path(), "alice", &["new", "parent"]);
    let child = run_mote_ok(temp.path(), "alice", &["new", "child"]);
    run_mote_ok(temp.path(), "alice", &["rel", "add", &child, &parent]);
    run_mote_ok(temp.path(), "alice", &["close", &child]);

    let doing = run_mote_ok(temp.path(), "alice", &["new", "doing"]);
    run_mote_ok(temp.path(), "alice", &["set", &doing, "status=doing"]);

    let dangling = run_mote_ok(temp.path(), "alice", &["new", "dangling"]);
    let target = run_mote_ok(temp.path(), "alice", &["new", "target"]);
    run_mote_ok(temp.path(), "alice", &["dep", "add", &dangling, &target]);
    run_mote_ok(temp.path(), "alice", &["rel", "add", &dangling, &target]);
    run_mote_ok(temp.path(), "alice", &["delete", &target]);

    let reserved = run_mote_ok(temp.path(), "alice", &["new", "reserved"]);
    run_mote_ok(
        temp.path(),
        "alice",
        &[
            "reserve",
            "src/audit.rs",
            "--issue",
            &reserved,
            "--ttl",
            "2h",
        ],
    );
    run_mote_ok(temp.path(), "alice", &["close", &reserved]);

    let open_reserved = run_mote_ok(temp.path(), "alice", &["new", "open reserved"]);
    run_mote_ok(
        temp.path(),
        "alice",
        &[
            "reserve",
            "src/open.rs",
            "--issue",
            &open_reserved,
            "--ttl",
            "2h",
        ],
    );

    let claimed = run_mote_ok(temp.path(), "alice", &["new", "claimed then deleted"]);
    run_mote_ok(temp.path(), "alice", &["claim", &claimed, "--ttl", "2h"]);
    run_mote_ok(temp.path(), "alice", &["delete", &claimed]);

    let zero_children = run_mote_ok(temp.path(), "alice", &["new", "zero children"]);
    let mixed_parent = run_mote_ok(temp.path(), "alice", &["new", "mixed parent"]);
    let mixed_closed = run_mote_ok(temp.path(), "alice", &["new", "mixed closed"]);
    let mixed_open = run_mote_ok(temp.path(), "alice", &["new", "mixed open"]);
    run_mote_ok(
        temp.path(),
        "alice",
        &["rel", "add", &mixed_closed, &mixed_parent],
    );
    run_mote_ok(
        temp.path(),
        "alice",
        &["rel", "add", &mixed_open, &mixed_parent],
    );
    run_mote_ok(temp.path(), "alice", &["close", &mixed_closed]);
    let before = store.list_op_filenames().unwrap();

    let output = run_mote(temp.path(), &["--json", "audit", "--fail-on", "never"]);
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| finding["code"].as_str().unwrap())
        .collect();
    for code in [
        "parent_all_children_closed",
        "doing_without_live_claim",
        "dangling_dependency",
        "dangling_relation",
        "orphaned_claim",
        "orphaned_reservation",
        "unassigned_with_live_coordination",
    ] {
        assert!(codes.contains(&code), "missing {code}: {codes:?}");
    }
    assert!(value["findings"].as_array().unwrap().iter().all(|finding| {
        finding["code"] != "parent_all_children_closed"
            || finding["subject"] != zero_children && finding["subject"] != mixed_parent
    }));
    let ordering: Vec<(u8, &str, &str)> = value["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|finding| {
            let rank = match finding["severity"].as_str().unwrap() {
                "error" => 0,
                "warning" => 1,
                "info" => 2,
                other => panic!("unexpected severity {other}"),
            };
            (
                rank,
                finding["code"].as_str().unwrap(),
                finding["subject"].as_str().unwrap(),
            )
        })
        .collect();
    let mut sorted = ordering.clone();
    sorted.sort();
    assert_eq!(
        ordering, sorted,
        "findings must have deterministic ordering"
    );
    assert_eq!(
        reducer::replay_store(&store).unwrap().beads[&parent]
            .status
            .as_str(),
        "open"
    );
    assert_eq!(store.list_op_filenames().unwrap(), before);
}

#[test]
fn idle_activity_is_advisory_and_does_not_change_a_live_lease() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    run_mote_ok(
        temp.path(),
        "idle",
        &["session", "start", "--as", "idle", "--ttl", "2h"],
    );
    let state = reducer::replay_store(&store).unwrap();
    let future = Timestamp::now()
        .checked_add(SignedDuration::from_secs(3_600))
        .unwrap();
    let report = run_audit(
        &store,
        &state,
        temp.path(),
        future,
        None,
        Some(60),
        AuditFailOn::Never,
    )
    .unwrap();
    let idle = report
        .findings
        .iter()
        .find(|finding| finding.code == "live_but_idle")
        .unwrap();
    assert_eq!(idle.subject, "idle");
    assert_eq!(idle.evidence["presence"]["state"], "live");
    assert!(idle.message.contains("no work or interaction"));
}

#[test]
fn stale_threshold_separates_live_idle_from_expired_recent_and_aged_requests() {
    let temp = TempDir::new().unwrap();
    let store = Store::init(temp.path()).unwrap();
    let at = |value: &str| value.parse::<Timestamp>().unwrap();

    publish_checked(
        &store,
        &make_session_start(
            "live-idle".into(),
            "sess-live-idle".into(),
            7_200,
            None,
            None,
            at("2030-01-01T00:00:00Z"),
        ),
    );
    publish_checked(
        &store,
        &make_session_start(
            "expired-recent".into(),
            "sess-expired-recent".into(),
            60,
            None,
            None,
            at("2030-01-01T00:00:00Z"),
        ),
    );
    publish_checked(
        &store,
        &make_create(
            "expired-recent".into(),
            ids::new_bead_id(),
            ScalarSet {
                title: Some("recent work after lease expiry".into()),
                ..Default::default()
            },
            at("2030-01-01T00:59:30Z"),
        ),
    );
    let request = ids::new_msg_id();
    publish_checked(
        &store,
        &make_msg_send(
            "sender".into(),
            request.clone(),
            "recipient".into(),
            None,
            None,
            "request".into(),
            "please review".into(),
            at("2030-01-01T00:00:00Z"),
        ),
    );
    let state = reducer::replay_store(&store).unwrap();
    let as_of = at("2030-01-01T01:00:00Z");
    let expired = mote::actor_status::actor_status(&state, "expired-recent", None, as_of, 60);
    assert_eq!(expired.presence.state, "expired");
    assert!(expired.activity.recent);

    let report = run_audit(
        &store,
        &state,
        temp.path(),
        as_of,
        None,
        Some(60),
        AuditFailOn::Never,
    )
    .unwrap();
    assert!(
        report
            .findings
            .iter()
            .any(|finding| { finding.code == "live_but_idle" && finding.subject == "live-idle" })
    );
    assert!(
        report.findings.iter().all(|finding| {
            finding.code != "live_but_idle" || finding.subject != "expired-recent"
        })
    );
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "aged_open_request" && finding.subject == request)
    );
}
