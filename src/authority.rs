//! Shared-store authority boundary. Once enabled, immutable admission records
//! freeze replay order; wall-clock timestamps no longer reorder accepted history.
//! All in-tree publishers take the same OS lock. Independent store replicas are
//! not a consensus group and must not be used as independent landing authorities.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::errors::{MoteError, MoteResult};
use crate::{ids::OpName, publish, repo::Store};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Admission {
    files: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PreparedOp {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl PreparedOp {
    pub(crate) fn new(op: &crate::op::Op) -> MoteResult<Self> {
        let (name, bytes) = publish::encode_op(op)?;
        Ok(Self {
            name: name.into_string(),
            bytes,
        })
    }
}

/// An exclusive, crash-released lock on the common store's publication boundary.
pub struct Writer<'a> {
    pub(crate) store: &'a Store,
    _lock: File,
}

pub fn directory(store: &Store) -> PathBuf {
    store.root().join("authority")
}

/// Highest authority format version this binary understands.
///
/// - 1: shared admission order, holder-checked handoff, checked landing.
/// - 2: the log may contain session-bound claims (claim op `v: 2`). Binaries
///   that only understand version 1 ignore the session binding and would
///   replay those claims differently, so they must refuse the store; they do,
///   because every replay checks this version first.
pub const AUTHORITY_VERSION: u32 = 2;

pub fn enabled(store: &Store) -> bool {
    directory(store).join("00000000000000000000.json").is_file()
}

/// Read-only description of the shared admission authority.  Callers use this
/// as a contract boundary, so it validates an enabled store before reporting
/// it as usable.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub schema: &'static str,
    pub store_id: String,
    pub enabled: bool,
    pub authority_version: u32,
    pub genesis_digest: Option<String>,
    pub capabilities: [&'static str; 3],
}

pub fn status(store: &Store) -> MoteResult<Status> {
    let format = store.read_format()?;
    let enabled = enabled(store);
    let genesis_digest = if enabled {
        let authority = format.authority.as_ref().ok_or_else(|| {
            MoteError::Other("authority genesis exists without a FORMAT binding".into())
        })?;
        if !(1..=AUTHORITY_VERSION).contains(&authority.version) {
            return Err(MoteError::Other(
                "unsupported authority format version".into(),
            ));
        }
        // list_op_filenames verifies both the immutable FORMAT binding and
        // every admitted operation without recovering or changing the store.
        store.list_op_filenames()?;
        let digest = blake3::hash(&fs::read(
            directory(store).join("00000000000000000000.json"),
        )?)
        .to_hex()
        .to_string();
        if digest != authority.genesis_hash {
            return Err(MoteError::Other("authority genesis mismatch".into()));
        }
        Some(digest)
    } else {
        if format.authority.is_some() {
            return Err(MoteError::Other(
                "authority format names a missing genesis journal".into(),
            ));
        }
        None
    };
    Ok(Status {
        schema: "mote.authority-status.v1",
        store_id: format.store_id,
        enabled,
        authority_version: if enabled {
            format.authority.as_ref().map_or(1, |a| a.version)
        } else {
            0
        },
        genesis_digest,
        capabilities: [
            "stable_claim_order",
            "holder_checked_handoff",
            "checked_landing_results",
        ],
    })
}

/// Atomically replace a journal and make both its bytes and directory durable.
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> MoteResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MoteError::Invalid("journal has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", ulid::Ulid::new()));
    publish::write_tmp_durable(&temporary, &serde_json::to_vec(value)?)?;
    fs::rename(&temporary, path)?;
    publish::fsync_dir(parent)
}

fn records(store: &Store) -> MoteResult<Vec<PathBuf>> {
    let mut paths = fs::read_dir(directory(store))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|p| {
        p.file_name().and_then(|s| s.to_str()).is_some_and(|s| {
            s.len() == 25 && s.ends_with(".json") && s[..20].bytes().all(|b| b.is_ascii_digit())
        })
    });
    paths.sort();
    for (index, path) in paths.iter().enumerate() {
        if path.file_name().unwrap() != format!("{index:020}.json").as_str() {
            return Err(MoteError::Other(
                "authority admission gap; restore the complete store".into(),
            ));
        }
    }
    Ok(paths)
}

/// Return the frozen publication order. Unjournaled imports fail closed.
/// A pending publication is invisible until its admission record is durable.
pub(crate) fn ordered_names(store: &Store, raw: Vec<String>) -> MoteResult<Vec<String>> {
    let format = store.read_format()?;
    if let Some(authority) = &format.authority {
        if !(1..=AUTHORITY_VERSION).contains(&authority.version) {
            return Err(MoteError::Other(
                "unsupported authority format version".into(),
            ));
        }
        let genesis = fs::read(directory(store).join("00000000000000000000.json"))?;
        if blake3::hash(&genesis).to_hex().as_str() != authority.genesis_hash {
            return Err(MoteError::Other(
                "authority genesis mismatch; restore the complete store".into(),
            ));
        }
    }
    if !enabled(store) {
        return Ok(raw);
    }
    let pending_path = directory(store).join("publication.json");
    // Read pending before the record snapshot: a concurrent writer can advance
    // either side, but cannot expose an unjournaled operation.
    let pending: Option<PreparedOp> = match fs::read(&pending_path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    for path in records(store)? {
        let admission: Admission = serde_json::from_slice(&fs::read(path)?)?;
        for (name, digest) in admission.files {
            crate::ids::parse(&name)?;
            if !seen.insert(name.clone()) {
                return Err(MoteError::Other("duplicate authority admission".into()));
            }
            let bytes = fs::read(store.ops_dir().join(&name))?;
            if blake3::hash(&bytes).to_hex().as_str() != digest {
                return Err(MoteError::Other(format!("authority op changed: {name}")));
            }
            names.push(name);
        }
    }
    for name in raw {
        if !seen.contains(&name)
            && pending
                .as_ref()
                .is_none_or(|p| format!("{}.json", p.name) != name)
        {
            return Err(MoteError::Other(format!(
                "unadmitted operation {name}; fenced stores require the shared Mote publisher"
            )));
        }
    }
    Ok(names)
}

impl<'a> Writer<'a> {
    pub fn acquire(store: &'a Store) -> MoteResult<Self> {
        fs::create_dir_all(store.local_dir())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(store.local_dir().join("publication.lock"))?;
        file.lock_exclusive()?;
        let writer = Self { store, _lock: file };
        if enabled(store) {
            writer.mark_format()?;
        }
        if store.read_format()?.authority.is_some() {
            // Validate the protocol and admitted prefix before recovery writes,
            // including when FORMAT names a missing or unsupported journal.
            store.list_op_filenames()?;
        }
        writer.recover_publication()?;
        Ok(writer)
    }

    /// Activate a single authority, retaining the exact existing replay prefix.
    pub fn enable(&self) -> MoteResult<()> {
        if enabled(self.store) {
            return self.mark_format();
        }
        let files = self
            .store
            .list_op_filenames()?
            .into_iter()
            .map(|name| {
                let bytes = fs::read(self.store.ops_dir().join(&name))?;
                Ok((name, blake3::hash(&bytes).to_hex().to_string()))
            })
            .collect::<MoteResult<Vec<_>>>()?;
        fs::create_dir_all(directory(self.store))?;
        write_json(
            &directory(self.store).join("00000000000000000000.json"),
            &Admission { files },
        )?;
        publish::fsync_dir(self.store.root())?;
        self.mark_format()
    }

    fn mark_format(&self) -> MoteResult<()> {
        let mut format = self.store.read_format()?;
        if format.authority.is_none() {
            format.authority = Some(crate::repo::AuthorityFormat {
                version: 1,
                genesis_hash: blake3::hash(&fs::read(
                    directory(self.store).join("00000000000000000000.json"),
                )?)
                .to_hex()
                .to_string(),
            });
            write_json(&self.store.format_path(), &format)?;
        }
        Ok(())
    }

    fn raise_format_version(&self, version: u32) -> MoteResult<()> {
        let mut format = self.store.read_format()?;
        let authority = format
            .authority
            .as_mut()
            .ok_or_else(|| MoteError::Other("authority enabled without a FORMAT binding".into()))?;
        if authority.version < version {
            authority.version = version;
            write_json(&self.store.format_path(), &format)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_no_landing(&self) -> MoteResult<()> {
        if directory(self.store).join("landing-active.json").exists() {
            return Err(MoteError::Rejected("landing recovery required; retry the recorded candidate land request before publishing other mutations".into()));
        }
        Ok(())
    }

    fn recover_publication(&self) -> MoteResult<()> {
        let path = directory(self.store).join("publication.json");
        if path.exists() {
            let prepared: PreparedOp = serde_json::from_slice(&fs::read(&path)?)?;
            self.finish_publication(&prepared)?;
        }
        Ok(())
    }

    fn finish_publication(&self, prepared: &PreparedOp) -> MoteResult<()> {
        let validated_name = OpName::from_string(prepared.name.clone())?;
        let filename = format!("{}.json", prepared.name);
        let op_path = self.store.ops_dir().join(&filename);
        if op_path.exists() {
            if fs::read(&op_path)? != prepared.bytes {
                return Err(MoteError::Other(
                    "publication recovery bytes disagree".into(),
                ));
            }
        } else {
            // A crash before link can leave the exact durable tmp file.
            let temp = self.store.tmp_dir().join(&filename);
            if temp.exists() {
                if fs::read(&temp)? != prepared.bytes {
                    return Err(MoteError::Other(
                        "publication temporary bytes disagree".into(),
                    ));
                }
                fs::hard_link(&temp, &op_path)?;
                publish::fsync_dir(&self.store.ops_dir())?;
                fs::remove_file(temp)?;
            } else {
                publish::publish_bytes_unlocked(self.store, &validated_name, &prepared.bytes)?;
            }
        }
        checkpoint("publication-linked")?;
        let all_records = records(self.store)?;
        let admitted = all_records
            .iter()
            .try_fold(false, |found, path| -> MoteResult<bool> {
                let record: Admission = serde_json::from_slice(&fs::read(path)?)?;
                Ok(found || record.files.iter().any(|(name, _)| *name == filename))
            })?;
        if !admitted {
            write_json(
                &directory(self.store).join(format!("{:020}.json", all_records.len())),
                &Admission {
                    files: vec![(filename, blake3::hash(&prepared.bytes).to_hex().to_string())],
                },
            )?;
        }
        checkpoint("publication-admitted")?;
        fs::remove_file(directory(self.store).join("publication.json"))?;
        publish::fsync_dir(&directory(self.store))
    }

    pub(crate) fn publish(&self, prepared: &PreparedOp) -> MoteResult<()> {
        if is_session_claim(&prepared.bytes) {
            // Fence version-1 readers before the first session-bound claim
            // becomes visible. Pre-authority binaries cannot be fenced.
            self.enable()?;
            self.raise_format_version(2)?;
        }
        if !enabled(self.store) {
            return publish::publish_bytes_unlocked(
                self.store,
                &OpName::from_string(prepared.name.clone())?,
                &prepared.bytes,
            );
        }
        // Validate the complete prefix before extending it.
        self.store.list_op_filenames()?;
        // Compound commands retain this writer. A preceding failed publication
        // must finish under the same lock before its recovery journal can be
        // replaced; an unrecoverable error leaves that exact journal intact.
        self.recover_publication()?;
        write_json(&directory(self.store).join("publication.json"), prepared)?;
        checkpoint("publication-prepared")?;
        self.finish_publication(prepared)
    }
}

fn is_session_claim(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v| {
        v.get("kind").and_then(|k| k.as_str()) == Some("claim")
            && v.get("session").is_some_and(|s| !s.is_null())
    })
}

/// Debug-build fault injection, used by process-level recovery courts. Production
/// release binaries do not consult environment-controlled failpoints.
pub(crate) fn checkpoint(name: &str) -> MoteResult<()> {
    #[cfg(debug_assertions)]
    {
        if std::env::var("MOTE_TEST_AUTHORITY_PAUSE").ok().as_deref() == Some(name) {
            if let Ok(path) = std::env::var("MOTE_TEST_AUTHORITY_SIGNAL") {
                fs::write(&path, name)?;
                while Path::new(&path).exists() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        if std::env::var("MOTE_TEST_AUTHORITY_FAIL").ok().as_deref() == Some(name) {
            return Err(MoteError::Other(format!("injected interruption at {name}")));
        }
    }
    let _ = name;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::{ScalarSet, make_create, make_note};
    use jiff::Timestamp;

    fn fixture() -> (tempfile::TempDir, Store, String) {
        let root = tempfile::TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        let issue = crate::ids::new_bead_id();
        publish::publish_op(
            &store,
            &make_create(
                "alice".into(),
                issue.clone(),
                ScalarSet {
                    title: Some("publication recovery".into()),
                    ..ScalarSet::default()
                },
                Timestamp::now(),
            ),
        )
        .unwrap();
        (root, store, issue)
    }

    fn note(issue: &str, text: &str) -> PreparedOp {
        PreparedOp::new(&make_note(
            "alice".into(),
            issue.into(),
            "note".into(),
            text.into(),
            Timestamp::now(),
        ))
        .unwrap()
    }

    fn linked_pending(store: &Store, prepared: &PreparedOp) {
        // The production journal and publisher establish the state immediately
        // after linking an operation, before its admission record is durable.
        write_json(&directory(store).join("publication.json"), prepared).unwrap();
        publish::publish_bytes_unlocked(
            store,
            &OpName::from_string(prepared.name.clone()).unwrap(),
            &prepared.bytes,
        )
        .unwrap();
    }

    #[test]
    fn reused_writer_recovers_linked_publication_before_next_operation() {
        let (_root, store, issue) = fixture();
        let writer = Writer::acquire(&store).unwrap();
        writer.enable().unwrap();
        let first = note(&issue, "pending note");
        linked_pending(&store, &first);
        let second = note(&issue, "next note");
        writer.publish(&second).unwrap();
        drop(writer);
        let names = store.list_op_filenames().unwrap();
        assert_eq!(
            &names[1..],
            &[
                format!("{}.json", first.name),
                format!("{}.json", second.name)
            ]
        );
        assert!(!directory(&store).join("publication.json").exists());
        let state = crate::reducer::replay_store(&store).unwrap();
        assert_eq!(state.beads[&issue].notes.len(), 2);
        Writer::acquire(&store).unwrap();
    }

    #[test]
    fn failed_same_writer_recovery_preserves_original_journal() {
        let (_root, store, issue) = fixture();
        let writer = Writer::acquire(&store).unwrap();
        writer.enable().unwrap();
        let first = note(&issue, "pending note");
        linked_pending(&store, &first);
        let mut damaged = first.clone();
        damaged.bytes.push(b' ');
        let path = directory(&store).join("publication.json");
        write_json(&path, &damaged).unwrap();
        let original = fs::read(&path).unwrap();
        let second = note(&issue, "must not publish");
        let error = writer.publish(&second).unwrap_err();
        assert!(
            error.to_string().contains("recovery bytes disagree"),
            "{error}"
        );
        assert_eq!(fs::read(path).unwrap(), original);
        assert!(
            !store
                .ops_dir()
                .join(format!("{}.json", second.name))
                .exists()
        );
    }
}
