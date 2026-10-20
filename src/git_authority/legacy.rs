//! A closed, pinned replay archive of one explicitly selected local history.
//! Capture is read-only under the publication lock; import never recovers a
//! journal, unions replicas, or interprets TTL events as remote ownership.

use super::protocol::{decode, encode, invalid};
use crate::{MoteError, MoteResult, Store, state::State};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    path::Path,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub ordinal: u64,
    pub name: String,
    pub path: String,
    pub digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub name: String,
    pub digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCut {
    pub schema: String,
    pub profile: String,
    pub store_id: String,
    pub order: String,
    pub authority_version: Option<u32>,
    pub authority_genesis: Option<String>,
    pub format_digest: String,
    pub entries: Vec<Entry>,
    pub admissions: Vec<Admission>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub schema: String,
    pub source: SourceCut,
    pub source_cut_digest: String,
    pub decisions_digest: String,
    pub final_state_digest: String,
    pub task_seed_digest: String,
    pub transition_digest: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub schema: String,
    pub source_cut_digest: String,
    /// Number of original inputs preceding the designated cleanup suffix.
    pub cleanup_start: u64,
    pub final_state_digest: String,
    pub task_seed_digest: String,
    pub ownership: String,
    pub consumed_tasks: BTreeSet<String>,
    pub consumed_sessions: BTreeSet<String>,
    pub consumed_reservations: BTreeSet<String>,
}
pub struct Archive {
    pub files: BTreeMap<String, Vec<u8>>,
    pub digest: String,
}
pub struct Verified {
    pub export: Export,
    pub transition: Transition,
    pub entries: Vec<(String, Vec<u8>)>,
    pub state: State,
}

fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn digest<T: Serialize>(domain: &str, value: &T) -> MoteResult<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(&encode(value)?);
    Ok(hasher.finalize().to_hex().to_string())
}
fn state_digest(profile: &str, state: &State) -> MoteResult<String> {
    digest(
        "mote.legacy-state.v1",
        &json!({"schema":"mote.legacy-state.v1", "profile":profile, "state":state}),
    )
}
fn task_seed(state: &State) -> Value {
    json!({"schema":"mote.legacy-task-seed.v1", "tasks":state.beads})
}
fn clean(state: &State) -> MoteResult<()> {
    if state.beads.values().any(|b| b.claim.is_some())
        || state
            .reservations
            .values()
            .any(|r| r.paths.iter().any(|p| !r.closed_paths.contains(p)))
        || state.sessions.values().any(|s| s.ended_op_id.is_none())
    {
        return invalid(
            "legacy ownership requires explicit claim releases, reservation closes and session ends before migration; expiry is insufficient",
        );
    }
    Ok(())
}
fn replay_evidence(
    entries: &[(String, Vec<u8>)],
    cleanup_start: u64,
) -> MoteResult<(State, Value)> {
    if cleanup_start > entries.len() as u64 {
        return invalid("cleanup boundary exceeds selected history");
    }
    let mut state = State::default();
    let mut decisions = Vec::new();
    for (ordinal, (name, bytes)) in entries.iter().enumerate() {
        let lengths: BTreeMap<_, _> = state
            .history
            .iter()
            .map(|(id, events)| (id.clone(), events.len()))
            .collect();
        let orphan_len = state.orphan_history.len();
        crate::reducer::apply_entry(&mut state, name, bytes);
        let mut placed = BTreeMap::<String, Value>::new();
        for (id, events) in &state.history {
            let start = lengths.get(id).copied().unwrap_or(0);
            if events.len() > start {
                placed.insert(id.clone(), serde_json::to_value(&events[start..])?);
            }
        }
        let orphan = &state.orphan_history[orphan_len..];
        if ordinal as u64 >= cleanup_start {
            let op: Value = decode(bytes)?;
            if !matches!(
                op.get("kind").and_then(Value::as_str),
                Some("release" | "reserve_close" | "session_end")
            ) || placed
                .values()
                .flat_map(|v| v.as_array().into_iter().flatten())
                .any(|e| e.get("accepted") != Some(&Value::Bool(true)))
                || orphan.iter().any(|e| !e.accepted)
            {
                return invalid(
                    "cleanup suffix must contain only accepted explicit release/close/end operations",
                );
            }
        }
        decisions.push(json!({"ordinal":ordinal,"entity_events":placed,"orphan_events":orphan}));
    }
    clean(&state)?;
    Ok((
        state,
        json!({"schema":"mote.legacy-decisions.v1", "inputs":decisions}),
    ))
}
fn canonical_file<T: serde::de::DeserializeOwned + Serialize>(
    files: &BTreeMap<String, Vec<u8>>,
    path: &str,
) -> MoteResult<T> {
    let bytes = files
        .get(path)
        .ok_or_else(|| MoteError::Invalid(format!("legacy archive is missing {path}")))?;
    let value: T = decode(bytes)?;
    if encode(&value)? != *bytes {
        return invalid("legacy archive metadata is not canonical");
    }
    Ok(value)
}

/// Capture exactly one stopped source. `cleanup_start` is the zero-based input
/// boundary; `None` designates an empty suffix for an already-clean source.
pub fn capture(store: &Store, cleanup_start: Option<u64>) -> MoteResult<Archive> {
    let profile = super::legacy_profile::current()?;
    capture_profile(store, cleanup_start, &profile)
}
fn capture_profile(
    store: &Store,
    cleanup_start: Option<u64>,
    profile: &str,
) -> MoteResult<Archive> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.local_dir().join("publication.lock"))?;
    lock.lock_exclusive()?;
    store.require_local_authority()?;
    crate::landing::ensure_migration_quiescent(store)?;
    if crate::authority::directory(store)
        .join("publication.json")
        .exists()
    {
        return invalid("pending local publication requires recovery before migration");
    }
    let status = crate::authority::status(store)?;
    if status.foreign_format_binding {
        return invalid(
            "foreign local authority binding requires explicit reconciliation before migration",
        );
    }
    let names = store.list_op_filenames()?;
    for entry in fs::read_dir(store.ops_dir())? {
        let entry = entry?;
        if !entry.file_type()?.is_file()
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|s| s.ends_with(".json"))
        {
            return invalid("unrecognized or nonregular legacy operation entry prevents migration");
        }
    }
    let mut files = BTreeMap::new();
    let format = fs::read(store.format_path())?;
    files.insert("legacy/format.bin".into(), format.clone());
    let mut entries = Vec::new();
    for (ordinal, name) in names.into_iter().enumerate() {
        let path = store.ops_dir().join(&name);
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return invalid("legacy operations must be regular files");
        }
        let bytes = fs::read(path)?;
        let archive_path = format!("legacy/ops/{ordinal:020}.bin");
        entries.push(Entry {
            ordinal: ordinal as u64,
            name,
            path: archive_path.clone(),
            digest: hash(&bytes),
        });
        files.insert(archive_path, bytes);
    }
    let mut admissions = Vec::new();
    if crate::authority::directory(store).exists() {
        let mut paths =
            fs::read_dir(crate::authority::directory(store))?.collect::<Result<Vec<_>, _>>()?;
        paths.sort_by_key(|e| e.file_name());
        for entry in paths {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| MoteError::Invalid("non-UTF8 authority metadata".into()))?;
            if name.starts_with("landing-") {
                continue;
            }
            if name.len() != 25
                || !name.ends_with(".json")
                || !name[..20].bytes().all(|b| b.is_ascii_digit())
                || !entry.file_type()?.is_file()
            {
                return invalid("unknown or pending local authority metadata prevents migration");
            }
            let bytes = fs::read(entry.path())?;
            admissions.push(Admission {
                name: name.clone(),
                digest: hash(&bytes),
            });
            files.insert(format!("legacy/admissions/{name}"), bytes);
        }
    }
    let source = SourceCut {
        schema: "mote.legacy-source-cut.v1".into(),
        profile: profile.into(),
        store_id: store.read_format()?.store_id,
        order: if status.enabled {
            "admitted"
        } else {
            "filename"
        }
        .into(),
        authority_version: status.enabled.then_some(status.authority_version),
        authority_genesis: status.genesis_digest,
        format_digest: hash(&format),
        entries,
        admissions,
    };
    build_archive(source, files, cleanup_start)
}
fn build_archive(
    source: SourceCut,
    mut files: BTreeMap<String, Vec<u8>>,
    cleanup_start: Option<u64>,
) -> MoteResult<Archive> {
    let entries = source
        .entries
        .iter()
        .map(|e| {
            Ok((
                e.name.clone(),
                files
                    .get(&e.path)
                    .ok_or_else(|| MoteError::Invalid("missing legacy input".into()))?
                    .clone(),
            ))
        })
        .collect::<MoteResult<Vec<_>>>()?;
    let boundary = cleanup_start.unwrap_or(entries.len() as u64);
    let (state, decisions) = replay_evidence(&entries, boundary)?;
    let seed = task_seed(&state);
    let source_digest = digest("mote.legacy-source-cut.v1", &source)?;
    let final_digest = state_digest(&source.profile, &state)?;
    let seed_digest = digest("mote.legacy-task-seed.v1", &seed)?;
    let transition = Transition {
        schema: "mote.legacy-transition.v1".into(),
        source_cut_digest: source_digest.clone(),
        cleanup_start: boundary,
        final_state_digest: final_digest.clone(),
        task_seed_digest: seed_digest.clone(),
        ownership: "explicit-release-v1".into(),
        consumed_tasks: state.beads.keys().cloned().collect(),
        consumed_sessions: state.sessions.keys().cloned().collect(),
        consumed_reservations: state.reservations.keys().cloned().collect(),
    };
    let export = Export {
        schema: "mote.legacy-export.v1".into(),
        source,
        source_cut_digest: source_digest,
        decisions_digest: digest("mote.legacy-decisions.v1", &decisions)?,
        final_state_digest: final_digest,
        task_seed_digest: seed_digest,
        transition_digest: digest("mote.legacy-transition.v1", &transition)?,
    };
    files.insert("legacy/decisions.json".into(), encode(&decisions)?);
    files.insert("legacy/task-seed.json".into(), encode(&seed)?);
    files.insert("legacy/transition.json".into(), encode(&transition)?);
    files.insert("legacy/export.json".into(), encode(&export)?);
    let archive = Archive {
        digest: digest("mote.legacy-export.v1", &export)?,
        files,
    };
    verify_profile(&archive.files, &archive.digest, &export.source.profile)?;
    Ok(archive)
}

pub fn verify(files: &BTreeMap<String, Vec<u8>>, expected_digest: &str) -> MoteResult<Verified> {
    let profile = super::legacy_profile::current()?;
    verify_profile(files, expected_digest, &profile)
}
fn verify_profile(
    files: &BTreeMap<String, Vec<u8>>,
    expected_digest: &str,
    profile: &str,
) -> MoteResult<Verified> {
    let export: Export = canonical_file(files, "legacy/export.json")?;
    // This check precedes legacy parsing/replay, including unknown-op decisions.
    if export.schema != "mote.legacy-export.v1"
        || export.source.schema != "mote.legacy-source-cut.v1"
        || export.source.profile != profile
    {
        return invalid(
            "unsupported legacy replay profile; use the retained matching migration binary",
        );
    }
    if digest("mote.legacy-export.v1", &export)? != expected_digest
        || digest("mote.legacy-source-cut.v1", &export.source)? != export.source_cut_digest
    {
        return invalid("legacy export/source digest mismatch");
    }
    let transition: Transition = canonical_file(files, "legacy/transition.json")?;
    let mut allowed = BTreeSet::<String>::from_iter(
        [
            "legacy/format.bin",
            "legacy/export.json",
            "legacy/decisions.json",
            "legacy/transition.json",
            "legacy/task-seed.json",
        ]
        .map(str::to_owned),
    );
    let source = &export.source;
    let format_bytes = files
        .get("legacy/format.bin")
        .ok_or_else(|| MoteError::Invalid("missing raw legacy FORMAT".into()))?;
    let format: crate::repo::Format = decode(format_bytes)?;
    if format.schema_version != 1
        || format.store_id != source.store_id
        || hash(format_bytes) != source.format_digest
    {
        return invalid("legacy FORMAT identity/digest mismatch");
    }
    if let Some(authority) = &format.authority {
        if source.order != "admitted"
            || source.authority_genesis.as_deref() != Some(authority.genesis_hash.as_str())
            || !source
                .authority_version
                .is_some_and(|version| authority.version > 0 && authority.version <= version)
        {
            return invalid("raw legacy FORMAT authority differs from selected source identity");
        }
    }
    let mut entries = Vec::new();
    let mut names = BTreeSet::new();
    for (ordinal, e) in source.entries.iter().enumerate() {
        if source.order == "admitted" {
            crate::ids::parse(&e.name)?;
        }
        if e.ordinal != ordinal as u64
            || e.path != format!("legacy/ops/{ordinal:020}.bin")
            || !e.name.ends_with(".json")
            || e.name.contains(['/', '\\'])
            || !names.insert(e.name.clone())
        {
            return invalid("invalid legacy input identity or ordinal");
        }
        allowed.insert(e.path.clone());
        let bytes = files
            .get(&e.path)
            .ok_or_else(|| MoteError::Invalid("missing legacy input bytes".into()))?;
        if hash(bytes) != e.digest {
            return invalid("legacy input digest mismatch");
        }
        entries.push((e.name.clone(), bytes.clone()));
    }
    let mut admitted = Vec::new();
    for (i, evidence) in source.admissions.iter().enumerate() {
        if evidence.name != format!("{i:020}.json") {
            return invalid("legacy admission gap/order mismatch");
        }
        let path = format!("legacy/admissions/{}", evidence.name);
        allowed.insert(path.clone());
        let bytes = files
            .get(&path)
            .ok_or_else(|| MoteError::Invalid("missing legacy admission".into()))?;
        if hash(bytes) != evidence.digest {
            return invalid("legacy admission digest mismatch");
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Record {
            files: Vec<(String, String)>,
        }
        let record: Record = decode(bytes)?;
        admitted.extend(record.files);
    }
    match source.order.as_str() {
        "filename"
            if source.admissions.is_empty()
                && source.authority_version.is_none()
                && source.authority_genesis.is_none()
                && entries.windows(2).all(|w| w[0].0 < w[1].0) => {}
        "admitted"
            if !source.admissions.is_empty()
                && source
                    .authority_version
                    .is_some_and(|v| (1..=crate::authority::AUTHORITY_VERSION).contains(&v))
                && source.authority_genesis.as_ref()
                    == source.admissions.first().map(|a| &a.digest)
                && admitted
                    == source
                        .entries
                        .iter()
                        .map(|e| (e.name.clone(), e.digest.clone()))
                        .collect::<Vec<_>>() => {}
        _ => return invalid("legacy source order/admission identity mismatch"),
    }
    if allowed != files.keys().cloned().collect() {
        return invalid("legacy archive has missing or extra files");
    }
    let (state, decisions) = replay_evidence(&entries, transition.cleanup_start)?;
    let seed = task_seed(&state);
    let actual_decisions: Value = canonical_file(files, "legacy/decisions.json")?;
    let actual_seed: Value = canonical_file(files, "legacy/task-seed.json")?;
    if actual_decisions != decisions
        || actual_seed != seed
        || digest("mote.legacy-decisions.v1", &decisions)? != export.decisions_digest
        || state_digest(profile, &state)? != export.final_state_digest
        || digest("mote.legacy-task-seed.v1", &seed)? != export.task_seed_digest
        || digest("mote.legacy-transition.v1", &transition)? != export.transition_digest
        || transition.schema != "mote.legacy-transition.v1"
        || transition.ownership != "explicit-release-v1"
        || transition.source_cut_digest != export.source_cut_digest
        || transition.final_state_digest != export.final_state_digest
        || transition.task_seed_digest != export.task_seed_digest
        || transition.consumed_tasks != state.beads.keys().cloned().collect()
        || transition.consumed_sessions != state.sessions.keys().cloned().collect()
        || transition.consumed_reservations != state.reservations.keys().cloned().collect()
    {
        return invalid("legacy pinned replay decisions, state, cleanup or task seed mismatch");
    }
    Ok(Verified {
        export,
        transition,
        entries,
        state,
    })
}

/// Write a new archive without replacing any pre-existing destination.
pub fn write_archive(archive: &Archive, destination: &Path) -> MoteResult<()> {
    verify(&archive.files, &archive.digest)?;
    write_archive_with_sync(archive, destination, &mut crate::publish::fsync_dir)
}
fn write_archive_with_sync(
    archive: &Archive,
    destination: &Path,
    sync: &mut dyn FnMut(&Path) -> MoteResult<()>,
) -> MoteResult<()> {
    if destination.exists() {
        return invalid("archive destination already exists");
    }
    fs::create_dir(destination)?;
    let destination = destination.canonicalize()?;
    let mut directories = BTreeSet::from([destination.clone()]);
    for (name, bytes) in &archive.files {
        if !name.starts_with("legacy/")
            || name.contains('\\')
            || Path::new(name)
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return invalid("invalid archive output path");
        }
        let path = destination.join(name);
        fs::create_dir_all(path.parent().expect("archive paths have parents"))?;
        let mut parent = path.parent();
        while let Some(path) = parent {
            if !path.starts_with(&destination) {
                break;
            }
            directories.insert(path.to_path_buf());
            parent = path.parent();
        }
        crate::publish::write_tmp_durable(&path, bytes)?;
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for path in directories {
        sync(&path)?;
    }
    sync(
        destination
            .parent()
            .ok_or_else(|| MoteError::Invalid("archive has no parent".into()))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(id: &str, kind: &str, actor: &str, ts: &str, extra: Value) -> (String, Vec<u8>) {
        let mut v = json!({"v":1,"op":id,"kind":kind,"actor":actor,"ts":ts});
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        (format!("{id}.json"), encode(&v).unwrap())
    }
    fn ttl_history() -> Vec<(String, Vec<u8>)> {
        vec![
            input(
                "001",
                "create",
                "a",
                "2026-01-01T00:00:00Z",
                json!({"entity":"bd-task","set":{"title":"retained","priority":1}}),
            ),
            input(
                "002",
                "claim",
                "a",
                "2026-01-01T00:00:01Z",
                json!({"entity":"bd-task","to":"a","ttl_s":1}),
            ),
            // The old owner never released. Legacy TTL takeover is preserved.
            input(
                "003",
                "claim",
                "b",
                "2026-01-01T00:00:03Z",
                json!({"entity":"bd-task","to":"b","ttl_s":1}),
            ),
            input(
                "004",
                "release",
                "b",
                "2026-01-01T00:00:03.5Z",
                json!({"entity":"bd-task","expect_claim":"003"}),
            ),
            input(
                "005",
                "future_unknown",
                "a",
                "2026-01-01T00:00:04Z",
                json!({"entity":"bd-task"}),
            ),
        ]
    }
    fn archive(entries: &[(String, Vec<u8>)], boundary: Option<u64>) -> Archive {
        let format = encode(&json!({"schema_version":1,"store_id":"st-fixture","created_at":"2026-01-01T00:00:00Z","default_ttl_s":{"claim":1800,"reservation":3600}})).unwrap();
        let mut files = BTreeMap::from([("legacy/format.bin".into(), format.clone())]);
        let entries = entries
            .iter()
            .enumerate()
            .map(|(ordinal, (name, bytes))| {
                let path = format!("legacy/ops/{ordinal:020}.bin");
                files.insert(path.clone(), bytes.clone());
                Entry {
                    ordinal: ordinal as u64,
                    name: name.clone(),
                    path,
                    digest: hash(bytes),
                }
            })
            .collect();
        build_archive(
            SourceCut {
                schema: "mote.legacy-source-cut.v1".into(),
                profile: "pinned-fixture".into(),
                store_id: "st-fixture".into(),
                order: "filename".into(),
                authority_version: None,
                authority_genesis: None,
                format_digest: hash(&format),
                entries,
                admissions: vec![],
            },
            files,
            boundary,
        )
        .unwrap()
    }
    #[test]
    fn ttl_takeover_and_unknown_rejection_survive_exact_pinned_replay() {
        let entries = ttl_history();
        let a = archive(&entries, None);
        let verified = verify_profile(&a.files, &a.digest, "pinned-fixture").unwrap();
        assert_eq!(verified.entries, entries);
        assert!(verified.state.was_accepted("002"));
        assert!(verified.state.was_accepted("003"));
        assert!(verified.state.was_accepted("004"));
        assert!(!verified.state.was_accepted("005"));
        assert!(verified.state.beads["bd-task"].claim.is_none());
        assert_eq!(
            verified.transition.consumed_tasks,
            BTreeSet::from(["bd-task".into()])
        );
        let original = crate::reducer::replay(entries);
        assert_eq!(encode(&original).unwrap(), encode(&verified.state).unwrap());
        assert!(verify_profile(&a.files, &a.digest, "newer-profile").is_err());
    }
    #[test]
    fn source_bytes_and_cleanup_boundary_are_independent_digest_inputs() {
        let mut entries = ttl_history();
        entries.pop();
        let a = archive(&entries, None);
        let b = archive(&entries, Some(3));
        let va = verify_profile(&a.files, &a.digest, "pinned-fixture").unwrap();
        let vb = verify_profile(&b.files, &b.digest, "pinned-fixture").unwrap();
        assert_eq!(va.export.source_cut_digest, vb.export.source_cut_digest);
        assert_ne!(va.export.transition_digest, vb.export.transition_digest);
        let mut source = va.export.source.clone();
        let mut files = a.files.clone();
        let bytes = files.get_mut("legacy/format.bin").unwrap();
        bytes.push(b'\n');
        source.format_digest = hash(bytes);
        let whitespace = build_archive(source, files, None).unwrap();
        let vw = verify_profile(&whitespace.files, &whitespace.digest, "pinned-fixture").unwrap();
        assert_ne!(va.export.source_cut_digest, vw.export.source_cut_digest);
        assert_eq!(va.export.final_state_digest, vw.export.final_state_digest);
    }
    #[test]
    fn closed_archive_refuses_missing_extra_altered_and_recomputed_false_evidence() {
        let a = archive(&ttl_history(), None);
        for name in a.files.keys() {
            let mut files = a.files.clone();
            files.remove(name);
            assert!(
                verify_profile(&files, &a.digest, "pinned-fixture").is_err(),
                "{name}"
            );
        }
        let mut extra = a.files.clone();
        extra.insert("local/credentials".into(), b"not exported".to_vec());
        assert!(verify_profile(&extra, &a.digest, "pinned-fixture").is_err());
        let mut changed = a.files.clone();
        changed
            .get_mut("legacy/ops/00000000000000000000.bin")
            .unwrap()
            .push(b' ');
        assert!(verify_profile(&changed, &a.digest, "pinned-fixture").is_err());
        let mut forged = a.files.clone();
        let false_decisions = json!({"schema":"mote.legacy-decisions.v1","inputs":[]});
        forged.insert(
            "legacy/decisions.json".into(),
            encode(&false_decisions).unwrap(),
        );
        let mut export: Export = canonical_file(&forged, "legacy/export.json").unwrap();
        export.decisions_digest = digest("mote.legacy-decisions.v1", &false_decisions).unwrap();
        forged.insert("legacy/export.json".into(), encode(&export).unwrap());
        let forged_digest = digest("mote.legacy-export.v1", &export).unwrap();
        assert!(verify_profile(&forged, &forged_digest, "pinned-fixture").is_err());
    }
    #[test]
    fn expired_claim_is_not_cleanup_and_tombstones_remain_consumed() {
        let mut entries = ttl_history();
        entries.truncate(3);
        assert!(replay_evidence(&entries, entries.len() as u64).is_err());
        entries = ttl_history();
        entries.pop();
        entries.push(input(
            "005",
            "delete",
            "a",
            "2026-01-01T00:00:05Z",
            json!({"entity":"bd-task"}),
        ));
        let a = archive(&entries, None);
        let verified = verify_profile(&a.files, &a.digest, "pinned-fixture").unwrap();
        assert!(verified.state.beads["bd-task"].is_deleted());
        assert!(verified.transition.consumed_tasks.contains("bd-task"));
    }

    #[test]
    fn capture_keeps_admitted_backdated_order_and_never_recovers_pending_source() {
        let td = tempfile::tempdir().unwrap();
        let store = Store::init(td.path()).unwrap();
        crate::authority::Writer::acquire(&store)
            .unwrap()
            .enable()
            .unwrap();
        let created = crate::publish::publish_value(&store,
            json!({"kind":"create","actor":"alice","entity":"bd-ordered","set":{"title":"ordered"}}),
            "2026-01-02T00:00:00Z".parse().unwrap()).unwrap();
        let backdated = crate::publish::publish_value(&store,
            json!({"kind":"note","actor":"alice","entity":"bd-ordered","note_kind":"progress","text":"accepted after create"}),
            "2026-01-01T00:00:00Z".parse().unwrap()).unwrap();
        crate::publish::publish_value(
            &store,
            json!({"kind":"unknown_future","actor":"alice"}),
            "2026-01-03T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let original_format = fs::read(store.format_path()).unwrap();
        let names = store.list_op_filenames().unwrap();
        assert_eq!(names[0], format!("{}.json", created.as_str()));
        assert_eq!(names[1], format!("{}.json", backdated.as_str()));
        let archive = capture_profile(&store, None, "same-test-binary").unwrap();
        let verified = verify_profile(&archive.files, &archive.digest, "same-test-binary").unwrap();
        assert!(verified.state.was_accepted(backdated.as_str()));
        let mut sorted = verified.entries.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert!(!crate::reducer::replay(sorted).was_accepted(backdated.as_str()));
        assert_eq!(
            verified
                .entries
                .iter()
                .map(|(n, _)| n.clone())
                .collect::<Vec<_>>(),
            names
        );
        let mut reordered = verified.export.source.clone();
        reordered.admissions.swap(0, 1);
        assert_ne!(
            digest("mote.legacy-source-cut.v1", &reordered).unwrap(),
            verified.export.source_cut_digest
        );
        assert_eq!(fs::read(store.format_path()).unwrap(), original_format);
        assert_eq!(store.list_op_filenames().unwrap(), names);
        let pending = crate::authority::directory(&store).join("publication.json");
        fs::write(&pending, b"malformed pending bytes").unwrap();
        assert!(capture_profile(&store, None, "same-test-binary").is_err());
        assert_eq!(fs::read(&pending).unwrap(), b"malformed pending bytes");
        assert_eq!(fs::read(store.format_path()).unwrap(), original_format);
    }

    #[test]
    fn damaged_admissions_landing_metadata_and_unadmitted_inputs_refuse_capture() {
        for damage in [
            "unadmitted",
            "missing-admitted",
            "active-landing",
            "malformed-landing",
        ] {
            let td = tempfile::tempdir().unwrap();
            let store = Store::init(td.path()).unwrap();
            crate::authority::Writer::acquire(&store)
                .unwrap()
                .enable()
                .unwrap();
            let op = crate::publish::publish_value(
                &store,
                json!({"kind":"create","actor":"alice","entity":"bd-a","set":{"title":"a"}}),
                "2026-01-01T00:00:00Z".parse().unwrap(),
            )
            .unwrap();
            match damage {
                "unadmitted" => {
                    fs::write(store.ops_dir().join("unadmitted.json"), b"{}").unwrap();
                }
                "missing-admitted" => {
                    fs::remove_file(store.ops_dir().join(format!("{}.json", op.as_str()))).unwrap();
                }
                "active-landing" => {
                    fs::write(
                        crate::authority::directory(&store).join("landing-active.json"),
                        b"{}",
                    )
                    .unwrap();
                }
                "malformed-landing" => {
                    fs::write(
                        crate::authority::directory(&store).join("landing-bad.json"),
                        b"{}",
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                capture_profile(&store, None, "same-test-binary").is_err(),
                "{damage}"
            );
        }
    }

    #[test]
    fn admitted_names_are_checked_after_all_archive_evidence_is_recomputed() {
        let a = archive(&ttl_history(), None);
        let original = verify_profile(&a.files, &a.digest, "pinned-fixture").unwrap();
        let mut source = original.export.source;
        let mut files = a.files;
        let record = encode(
            &json!({"files":source.entries.iter().map(|e|(&e.name,&e.digest)).collect::<Vec<_>>()}),
        )
        .unwrap();
        let record_digest = hash(&record);
        source.order = "admitted".into();
        source.authority_version = Some(1);
        source.authority_genesis = Some(record_digest.clone());
        source.admissions = vec![Admission {
            name: "00000000000000000000.json".into(),
            digest: record_digest,
        }];
        files.insert("legacy/admissions/00000000000000000000.json".into(), record);
        // All state/decision/transition/export digests are rebuilt, so refusal
        // discriminates impossible admitted input names rather than hash damage.
        assert!(build_archive(source, files, None).is_err());
    }

    #[test]
    fn raw_format_authority_matches_genesis_and_may_have_an_older_version() {
        let td = tempfile::tempdir().unwrap();
        let store = Store::init(td.path()).unwrap();
        crate::authority::Writer::acquire(&store)
            .unwrap()
            .enable()
            .unwrap();
        crate::publish::publish_value(
            &store,
            json!({"kind":"create","actor":"alice","entity":"bd-test","set":{"title":"test"}}),
            "2026-01-01T00:00:00Z".parse().unwrap(),
        )
        .unwrap();
        let original = capture_profile(&store, None, "test-profile").unwrap();
        let verified = verify_profile(&original.files, &original.digest, "test-profile").unwrap();
        for (genesis, version, valid) in [
            (
                verified.export.source.authority_genesis.clone().unwrap(),
                1,
                true,
            ),
            ("0".repeat(64), 1, false),
            (
                verified.export.source.authority_genesis.clone().unwrap(),
                3,
                false,
            ),
        ] {
            let mut source = verified.export.source.clone();
            source.authority_version = Some(2);
            let mut files = original.files.clone();
            let mut format: Value = decode(&files["legacy/format.bin"]).unwrap();
            format["authority"] = json!({"version":version,"genesis_hash":genesis});
            let bytes = encode(&format).unwrap();
            source.format_digest = hash(&bytes);
            files.insert("legacy/format.bin".into(), bytes);
            assert_eq!(build_archive(source, files, None).is_ok(), valid);
        }
        let filename = archive(&ttl_history(), None);
        let verified = verify_profile(&filename.files, &filename.digest, "pinned-fixture").unwrap();
        let mut source = verified.export.source;
        let mut files = filename.files;
        let mut format: Value = decode(&files["legacy/format.bin"]).unwrap();
        format["authority"] = json!({"version":1,"genesis_hash":"0".repeat(64)});
        let bytes = encode(&format).unwrap();
        source.format_digest = hash(&bytes);
        files.insert("legacy/format.bin".into(), bytes);
        assert!(build_archive(source, files, None).is_err());
    }

    #[test]
    fn archive_syncs_nested_directories_and_parent_and_propagates_sync_failure() {
        let td = tempfile::tempdir().unwrap();
        let a = archive(&ttl_history(), None);
        let destination = td.path().join("archive");
        let mut synced = Vec::new();
        write_archive_with_sync(&a, &destination, &mut |path| {
            synced.push(path.to_path_buf());
            Ok(())
        })
        .unwrap();
        let canonical = destination.canonicalize().unwrap();
        assert!(synced.contains(&canonical.join("legacy/ops")));
        assert!(synced.contains(&canonical.join("legacy")));
        assert_eq!(synced.last().unwrap(), canonical.parent().unwrap());
        assert!(
            synced
                .windows(2)
                .all(|w| w[0].components().count() >= w[1].components().count())
        );
        let failed = td.path().join("failed");
        let result: MoteResult<&str> = (|| {
            write_archive_with_sync(&a, &failed, &mut |path| {
                if path.ends_with("legacy/ops") {
                    invalid("injected directory sync failure")
                } else {
                    Ok(())
                }
            })?;
            Ok("successful export receipt")
        })();
        assert!(result.is_err());
    }
}
