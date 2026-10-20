//! Full-chain validation. No prepared proposal becomes authoritative here.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::planner::SyntheticState;
use super::protocol::{
    Manifest, Outcome, Transaction, decode, encode, invalid, transaction_path, valid_oid,
};
use super::transport::{GitBlob, GitCommit};
use crate::MoteResult;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub endpoint_digest: String,
    pub branch_ref: String,
    pub store_id: String,
    pub generation: u64,
    pub root: String,
}

impl Binding {
    pub fn check_endpoint(&self, endpoint_digest: &str, branch_ref: &str) -> MoteResult<()> {
        if self.version != 1
            || self.endpoint_digest != endpoint_digest
            || self.branch_ref != branch_ref
        {
            return invalid("authority endpoint or branch differs from the pinned binding");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub commit: String,
    pub sequence: u64,
    pub request_digest: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub head: String,
    pub root: String,
    pub manifest: Manifest,
    pub state: SyntheticState,
    pub receipts: BTreeMap<String, Receipt>,
    pub files: BTreeMap<String, Vec<u8>>,
}

/// Validate commits in root-to-head order, optionally requiring a previously
/// confirmed head to remain in the chain. A missing/rewritten prefix is refused.
pub fn validate_chain(
    commits: &[GitCommit],
    binding: Option<&Binding>,
    previous_head: Option<&str>,
) -> MoteResult<Snapshot> {
    let Some(root) = commits.first() else {
        return invalid("authority history is empty");
    };
    if !valid_oid(&root.oid) || !root.parents.is_empty() {
        return invalid("invalid authority root");
    }
    let blob = root
        .files
        .get("manifest.json")
        .ok_or_else(|| crate::MoteError::Invalid("authority root has no manifest".into()))?;
    check_blob(blob)?;
    let manifest: Manifest = decode(&blob.bytes)?;
    manifest.validate()?;
    if encode(&manifest)? != blob.bytes {
        return invalid("authority manifest is not canonical JSON");
    }
    let mut state = SyntheticState::default();
    let expected_root_files = if manifest.legacy_digest.is_some() {
        BTreeSet::from(["manifest.json", "legacy.pack"])
    } else {
        BTreeSet::from(["manifest.json"])
    };
    if root
        .files
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_root_files
    {
        return invalid("authority root has an invalid file set");
    }
    if let Some(binding) = binding {
        if binding.version != 1
            || binding.root != root.oid
            || binding.store_id != manifest.store_id
            || binding.generation != manifest.generation
        {
            return invalid("authority root, store, generation or protocol differs from binding");
        }
    }
    if let Some(digest) = &manifest.legacy_digest {
        let pack = root
            .files
            .get("legacy.pack")
            .ok_or_else(|| crate::MoteError::Invalid("migrated root has no legacy pack".into()))?;
        check_blob(pack)?;
        let archive = super::legacy::unpack(&pack.bytes)?;
        let verified = super::legacy::verify(&archive, digest)?;
        if verified.export.source.store_id != manifest.store_id {
            return invalid("migrated archive source store differs from authority manifest");
        }
        if manifest.capabilities == super::protocol::MIGRATED_COMMUNICATION_CAPABILITIES {
            super::legacy_communications::validate(&verified)?;
        }
        state.coordination = super::coordination::CoordinationState::from_legacy(verified)?;
    }
    let mut seen = BTreeSet::from([root.oid.as_str()]);
    let mut previous = root;
    let mut receipts = BTreeMap::new();
    for commit in &commits[1..] {
        if !valid_oid(&commit.oid)
            || commit.oid.len() != root.oid.len()
            || !seen.insert(&commit.oid)
            || commit.parents != [previous.oid.clone()]
        {
            return invalid("authority history must be a unique single-parent chain");
        }
        let Some(sequence) = state.sequence.checked_add(1) else {
            return invalid("authority sequence overflow");
        };
        let path = transaction_path(sequence);
        if commit.files.len() != previous.files.len() + 1
            || !commit.files.contains_key(&path)
            || previous
                .files
                .iter()
                .any(|(name, blob)| commit.files.get(name) != Some(blob))
        {
            return invalid(
                "authority append changed history or did not add exactly its next transaction",
            );
        }
        let blob = &commit.files[&path];
        check_blob(blob)?;
        let transaction: Transaction = decode(&blob.bytes)?;
        manifest.allows(&transaction.request.command)?;
        if encode(&transaction)? != blob.bytes {
            return invalid("authority transaction is not canonical JSON");
        }
        if receipts.contains_key(&transaction.request.request_id) {
            return invalid("authority contains more than one receipt for a request ID");
        }
        state = transaction.validate(&state, &previous.oid)?;
        receipts.insert(
            transaction.request.request_id.clone(),
            Receipt {
                commit: commit.oid.clone(),
                sequence,
                request_digest: transaction.request_digest,
                outcome: transaction.outcome,
            },
        );
        previous = commit;
    }
    if previous_head.is_some_and(|head| !seen.contains(head)) {
        return invalid("authority observation is stale or rewrote previously confirmed history");
    }
    Ok(Snapshot {
        head: previous.oid.clone(),
        root: root.oid.clone(),
        manifest,
        state,
        receipts,
        files: previous
            .files
            .iter()
            .map(|(name, blob)| (name.clone(), blob.bytes.clone()))
            .collect(),
    })
}

fn check_blob(blob: &GitBlob) -> MoteResult<()> {
    if blob.mode != "100644" || !valid_oid(&blob.oid) {
        return invalid("authority data must be a regular non-executable Git blob");
    }
    Ok(())
}
