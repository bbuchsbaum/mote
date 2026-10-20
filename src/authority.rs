//! Shared-store authority boundary. Once enabled, immutable admission records
//! freeze replay order; wall-clock timestamps no longer reorder accepted history.
//! All in-tree publishers take the same OS lock. Independent store replicas are
//! not a consensus group and must not be used as independent landing authorities.
//!
//! Authority is per replica. `ops/` may be replicated (for example through
//! Git); `authority/` and the activation binding in `local/authority.json`
//! never are. Operations that arrive from another replica stay unadmitted, and
//! fail closed on read, until `mote admit` appends them after the local prefix.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::errors::{MoteError, MoteResult};
use crate::repo::AuthorityFormat;
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

/// The per-replica activation binding. It lives beside the publication lock in
/// `local/`, which is never replicated, instead of in the tracked FORMAT.json.
pub fn binding_path(store: &Store) -> PathBuf {
    store.local_dir().join("authority.json")
}

/// Highest authority format version this binary understands.
///
/// - 1: shared admission order, holder-checked handoff, checked landing.
/// - 2: the log may contain session-bound claims (claim op `v: 2`). Binaries
///   that only understand version 1 ignore the session binding and would
///   replay those claims differently, so they must refuse the store. They
///   check the version only in FORMAT.json, so they are fenced out only while
///   a replica's binding is also recorded there (an unmigrated store); see
///   `binding`.
pub const AUTHORITY_VERSION: u32 = 2;

pub fn enabled(store: &Store) -> bool {
    directory(store).join("00000000000000000000.json").is_file()
}

/// Where this replica's authority binding was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingLocation {
    /// `local/authority.json`, written by this binary.
    Local,
    /// The legacy `authority` field of FORMAT.json, written by earlier binaries.
    Format,
}

/// The effective per-replica binding of an authority genesis.
#[derive(Debug, Clone)]
pub struct Binding {
    pub version: u32,
    pub genesis_hash: String,
    pub location: BindingLocation,
    /// FORMAT.json names a different genesis than this replica's local binding:
    /// per-replica state that reached this replica through a shared file.
    pub foreign_format: bool,
}

fn read_local_binding(store: &Store) -> MoteResult<Option<AuthorityFormat>> {
    match fs::read(binding_path(store)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Resolve this replica's binding without validating or changing the store.
///
/// The local binding governs. A legacy FORMAT.json binding for the same
/// genesis is the pre-migration copy of it; an older binary may have raised
/// only that copy's version, so the higher version wins. A FORMAT.json binding
/// for another genesis is foreign and never governs a locally bound replica.
pub fn binding(store: &Store) -> MoteResult<Option<Binding>> {
    let legacy = store.read_format()?.authority;
    Ok(match (read_local_binding(store)?, legacy) {
        (Some(local), Some(format)) if format.genesis_hash == local.genesis_hash => Some(Binding {
            version: local.version.max(format.version),
            genesis_hash: local.genesis_hash,
            location: BindingLocation::Local,
            foreign_format: false,
        }),
        (Some(local), format) => Some(Binding {
            version: local.version,
            genesis_hash: local.genesis_hash,
            location: BindingLocation::Local,
            foreign_format: format.is_some(),
        }),
        (None, Some(format)) => Some(Binding {
            version: format.version,
            genesis_hash: format.genesis_hash,
            location: BindingLocation::Format,
            foreign_format: false,
        }),
        (None, None) => None,
    })
}

pub(crate) fn is_bound(store: &Store) -> MoteResult<bool> {
    Ok(binding(store)?.is_some())
}

const FOREIGN_FORMAT_REMEDY: &str = "restore the complete store, or, if FORMAT.json's \
     authority binding was replicated from another clone, run \
     `mote authority migrate --discard-foreign-format`";

fn genesis_digest(store: &Store) -> MoteResult<Option<String>> {
    match fs::read(directory(store).join("00000000000000000000.json")) {
        Ok(bytes) => Ok(Some(blake3::hash(&bytes).to_hex().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Check the binding's version and that it names this replica's genesis.
fn verify_binding(store: &Store, binding: &Binding) -> MoteResult<()> {
    if !(1..=AUTHORITY_VERSION).contains(&binding.version) {
        return Err(MoteError::Other(
            "unsupported authority format version".into(),
        ));
    }
    let digest = genesis_digest(store)?;
    if digest.as_deref() == Some(binding.genesis_hash.as_str()) {
        return Ok(());
    }
    let problem = if digest.is_none() {
        "authority binding names a missing genesis journal"
    } else {
        "authority genesis mismatch"
    };
    Err(MoteError::Other(match binding.location {
        BindingLocation::Format => format!("{problem}; {FOREIGN_FORMAT_REMEDY}"),
        BindingLocation::Local => format!("{problem}; restore the complete store"),
    }))
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
    /// Where this replica's binding lives; `format` means not yet migrated.
    pub binding: Option<BindingLocation>,
    /// FORMAT.json carries another replica's binding; see `authority migrate`.
    pub foreign_format_binding: bool,
    pub capabilities: [&'static str; 3],
}

pub fn status(store: &Store) -> MoteResult<Status> {
    let format = store.read_format()?;
    let enabled = enabled(store);
    let binding = binding(store)?;
    if let Some(binding) = &binding {
        verify_binding(store, binding)?;
    }
    let genesis_digest = if enabled {
        let binding = binding.as_ref().ok_or_else(|| {
            MoteError::Other(
                "authority genesis exists without a replica binding; run `mote authority enable`"
                    .into(),
            )
        })?;
        // list_op_filenames verifies both the binding and every admitted
        // operation without recovering or changing the store.
        store.list_op_filenames()?;
        Some(binding.genesis_hash.clone())
    } else {
        None
    };
    Ok(Status {
        schema: "mote.authority-status.v1",
        store_id: format.store_id,
        enabled,
        authority_version: if enabled {
            binding.as_ref().map_or(1, |b| b.version)
        } else {
            0
        },
        genesis_digest,
        binding: binding.as_ref().map(|b| b.location),
        foreign_format_binding: binding.as_ref().is_some_and(|b| b.foreign_format),
        capabilities: [
            "stable_claim_order",
            "holder_checked_handoff",
            "checked_landing_results",
        ],
    })
}

/// Atomically replace a journal and make both its bytes and directory durable.
pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> MoteResult<()> {
    write_bytes(path, &serde_json::to_vec(value)?)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> MoteResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MoteError::Invalid("journal has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".{}.tmp", ulid::Ulid::new()));
    publish::write_tmp_durable(&temporary, bytes)?;
    fs::rename(&temporary, path)?;
    publish::fsync_dir(parent)
}

/// Rewrite FORMAT.json in the layout `init` creates, so that removing the
/// legacy binding restores the file a repository may have committed.
fn write_format(store: &Store, format: &crate::repo::Format) -> MoteResult<()> {
    write_bytes(&store.format_path(), &serde_json::to_vec_pretty(format)?)
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

/// The admitted prefix, validated, plus the op files this replica has not
/// admitted. A pending publication is in neither list.
struct Scan {
    admitted: Vec<String>,
    unadmitted: Vec<String>,
}

fn scan(store: &Store, raw: Vec<String>) -> MoteResult<Scan> {
    if let Some(binding) = binding(store)? {
        verify_binding(store, &binding)?;
    }
    if !enabled(store) {
        return Ok(Scan {
            admitted: raw,
            unadmitted: Vec::new(),
        });
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
            let bytes = match fs::read(store.ops_dir().join(&name)) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(MoteError::Other(format!(
                        "admitted operation {name} is missing from ops/; restore it \
                         (for example by checking out a commit that contains it); \
                         a fenced replica cannot forget admitted history"
                    )));
                }
                Err(e) => return Err(e.into()),
            };
            if blake3::hash(&bytes).to_hex().as_str() != digest {
                return Err(MoteError::Other(format!(
                    "authority op changed: {name}; ops/ holds different bytes under an \
                     admitted operation id; restore the admitted bytes"
                )));
            }
            names.push(name);
        }
    }
    let unadmitted = raw
        .into_iter()
        .filter(|name| {
            !seen.contains(name)
                && pending
                    .as_ref()
                    .is_none_or(|p| format!("{}.json", p.name) != *name)
        })
        .collect();
    Ok(Scan {
        admitted: names,
        unadmitted,
    })
}

/// Return the frozen publication order. Unadmitted operations fail closed.
/// A pending publication is invisible until its admission record is durable.
///
/// While a landing needs recovery, unadmitted operations are invisible instead:
/// no other writer, `admit` included, may extend the prefix that the landing
/// decision bound, so failing here would leave the landing unrecoverable.
pub(crate) fn ordered_names(store: &Store, raw: Vec<String>) -> MoteResult<Vec<String>> {
    let scan = scan(store, raw)?;
    if landing_active(store) {
        return Ok(scan.admitted);
    }
    if let Some(name) = scan.unadmitted.first() {
        return Err(MoteError::Other(format!(
            "unadmitted operation {name} ({} unadmitted in total); this replica has not \
             admitted it, for example because it arrived through Git; run `mote admit` \
             to validate and admit it",
            scan.unadmitted.len()
        )));
    }
    Ok(scan.admitted)
}

fn landing_active(store: &Store) -> bool {
    directory(store).join("landing-active.json").exists()
}

/// Result of [`Writer::admit`].
#[derive(Debug, Clone, Serialize)]
pub struct AdmitReport {
    pub schema: &'static str,
    pub enabled: bool,
    /// Newly admitted op filenames, in admission (op-id) order.
    pub admitted: Vec<String>,
    /// Newly admitted ops whose id sorts before the last previously admitted
    /// op: concurrent history that replays after this replica's prefix.
    pub out_of_order: Vec<String>,
    /// Admission record written, if any.
    pub record: Option<String>,
    pub admitted_total: usize,
}

impl<'a> Writer<'a> {
    pub fn acquire(store: &'a Store) -> MoteResult<Self> {
        Ok(Self::acquire_with(store, false, false)?.0)
    }

    /// Like [`Writer::acquire`], but tolerate unadmitted operations: the
    /// admitted prefix is still validated before anything is written.
    pub fn acquire_for_admission(store: &'a Store) -> MoteResult<Self> {
        Ok(Self::acquire_with(store, true, false)?.0)
    }

    /// Like [`Writer::acquire`], but first drop a FORMAT.json binding that
    /// does not name this replica's genesis, returning whether one was
    /// dropped. The caller asserts that the binding was replicated from
    /// another clone; this replica's own binding is never dropped.
    pub fn acquire_discarding_foreign_format(store: &'a Store) -> MoteResult<(Self, bool)> {
        Self::acquire_with(store, false, true)
    }

    fn acquire_with(
        store: &'a Store,
        admitting: bool,
        discard_foreign: bool,
    ) -> MoteResult<(Self, bool)> {
        store.require_local_authority()?;
        Self::acquire_after_mode_check(store, admitting, discard_foreign)
    }

    fn acquire_after_mode_check(
        store: &'a Store,
        admitting: bool,
        discard_foreign: bool,
    ) -> MoteResult<(Self, bool)> {
        // Neither per-replica directory is replicated: a fresh Git clone of a
        // versioned store has `ops/` but no `local/` or `tmp/`.
        fs::create_dir_all(store.local_dir())?;
        fs::create_dir_all(store.tmp_dir())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(store.local_dir().join("publication.lock"))?;
        file.lock_exclusive()?;
        // A mode transition holds this same lock. A writer that waited for it
        // must not use the mode it observed before waiting.
        store.require_local_authority()?;
        let writer = Self { store, _lock: file };
        let discarded = discard_foreign && writer.discard_foreign_format()?;
        if enabled(store) {
            writer.bind_local()?;
        }
        if is_bound(store)? {
            // Validate the protocol and admitted prefix before recovery writes,
            // including when the binding names a missing or unsupported journal.
            if admitting {
                scan(store, crate::repo::raw_op_filenames(store)?)?;
            } else {
                store.list_op_filenames()?;
            }
        }
        writer.recover_publication()?;
        Ok((writer, discarded))
    }

    /// Activate a single authority, retaining the exact existing replay prefix.
    pub fn enable(&self) -> MoteResult<()> {
        if enabled(self.store) {
            return self.bind_local();
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
        self.bind_local()
    }

    /// Record this replica's binding in `local/`. A legacy FORMAT.json binding
    /// for the same genesis is copied with its version, so migration never
    /// changes the genesis digest; a mismatched one is never copied, and
    /// validation reports it.
    fn bind_local(&self) -> MoteResult<()> {
        if read_local_binding(self.store)?.is_some() {
            return Ok(());
        }
        let Some(genesis_hash) = genesis_digest(self.store)? else {
            return Ok(());
        };
        let version = match self.store.read_format()?.authority {
            Some(format) if format.genesis_hash == genesis_hash => format.version,
            Some(_) => return Ok(()),
            None => 1,
        };
        write_json(
            &binding_path(self.store),
            &AuthorityFormat {
                version,
                genesis_hash,
            },
        )
    }

    fn raise_format_version(&self, version: u32) -> MoteResult<()> {
        let mut local = read_local_binding(self.store)?.ok_or_else(|| {
            MoteError::Other("authority enabled without a replica binding".into())
        })?;
        if local.version < version {
            local.version = version;
            write_json(&binding_path(self.store), &local)?;
        }
        // Until a store is migrated, earlier binaries read only FORMAT.json;
        // keep fencing them out of a log they would replay differently.
        let mut format = self.store.read_format()?;
        if let Some(legacy) = format.authority.as_mut() {
            if legacy.genesis_hash == local.genesis_hash && legacy.version < version {
                legacy.version = version;
                write_format(self.store, &format)?;
            }
        }
        Ok(())
    }

    /// Remove a FORMAT.json binding that does not name this replica's genesis.
    fn discard_foreign_format(&self) -> MoteResult<bool> {
        let mut format = self.store.read_format()?;
        let Some(legacy) = &format.authority else {
            return Ok(false);
        };
        // Judge against the genesis on disk; without one, a binding that
        // matches this replica's local binding is its own (authority/ was
        // lost) and stays, so the replica keeps failing closed.
        let foreign = match genesis_digest(self.store)? {
            Some(digest) => digest != legacy.genesis_hash,
            None => read_local_binding(self.store)?
                .is_none_or(|local| local.genesis_hash != legacy.genesis_hash),
        };
        if foreign {
            format.authority = None;
            write_format(self.store, &format)?;
        }
        Ok(foreign)
    }

    /// Finish moving this replica's binding out of FORMAT.json: once the local
    /// binding is durable, remove the legacy copy for the same genesis.
    /// Returns whether FORMAT.json changed.
    pub fn migrate(&self) -> MoteResult<bool> {
        if enabled(self.store) {
            self.bind_local()?;
        }
        let Some(binding) = binding(self.store)? else {
            return Ok(false);
        };
        if binding.foreign_format {
            return Err(MoteError::Rejected(format!(
                "FORMAT.json binds another replica's authority genesis; {FOREIGN_FORMAT_REMEDY}"
            )));
        }
        let mut format = self.store.read_format()?;
        if binding.location != BindingLocation::Local || format.authority.is_none() {
            return Ok(false);
        }
        // Keep any version an earlier binary raised only in FORMAT.json.
        self.raise_format_version(binding.version)?;
        format.authority = None;
        write_format(self.store, &format)?;
        Ok(true)
    }

    pub(crate) fn ensure_no_landing(&self) -> MoteResult<()> {
        if directory(self.store).join("landing-active.json").exists() {
            return Err(MoteError::Rejected("landing recovery required; retry the recorded candidate land request before publishing other mutations".into()));
        }
        Ok(())
    }

    /// Admit operations present in `ops/` but not yet admitted by this
    /// replica, such as ops pulled from another clone. Each is validated as
    /// `fsck` would; any defect fails closed before anything is written. The
    /// batch is appended in op-id order as one atomic admission record after
    /// the existing prefix, which is never reordered.
    pub fn admit(&self) -> MoteResult<AdmitReport> {
        if landing_active(self.store) {
            return Err(MoteError::Rejected(
                "landing recovery required; retry the recorded candidate land request, \
                 then run `mote admit`"
                    .into(),
            ));
        }
        if !enabled(self.store) {
            return Ok(AdmitReport {
                schema: "mote.admit.v1",
                enabled: false,
                admitted: Vec::new(),
                out_of_order: Vec::new(),
                record: None,
                admitted_total: 0,
            });
        }
        let scan = scan(self.store, crate::repo::raw_op_filenames(self.store)?)?;
        let mut files = Vec::new();
        let mut session_claim = false;
        for name in &scan.unadmitted {
            let bytes = fs::read(self.store.ops_dir().join(name))?;
            // An op this binary cannot parse (a newer kind) is still admitted:
            // replay records it as rejected history, as it would for a local
            // publication. Integrity defects fail closed.
            let defects: Vec<_> = crate::fsck::check_op(name, &bytes)
                .into_iter()
                .filter(|defect| !matches!(defect, crate::fsck::OpDefect::Shape(_)))
                .collect();
            if !defects.is_empty() {
                let defects = defects
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(MoteError::Rejected(format!(
                    "refusing to admit {name}: {defects}; nothing was admitted"
                )));
            }
            session_claim |= is_session_claim(&bytes);
            files.push((name.clone(), blake3::hash(&bytes).to_hex().to_string()));
        }
        let last = scan.admitted.last().cloned();
        let out_of_order = scan
            .unadmitted
            .iter()
            .filter(|name| last.as_ref().is_some_and(|last| *name < last))
            .cloned()
            .collect();
        let record = if files.is_empty() {
            None
        } else {
            if session_claim {
                // Fence version-1 readers before the claim becomes visible.
                self.raise_format_version(2)?;
            }
            let path =
                directory(self.store).join(format!("{:020}.json", records(self.store)?.len()));
            write_json(&path, &Admission { files })?;
            Some(path.display().to_string())
        };
        Ok(AdmitReport {
            schema: "mote.admit.v1",
            enabled: true,
            admitted_total: scan.admitted.len() + scan.unadmitted.len(),
            admitted: scan.unadmitted,
            out_of_order,
            record,
        })
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
    #[test]
    fn pending_git_setup_is_rechecked_inside_the_publication_lock() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::repo::Store::init(root.path()).unwrap();
        store.require_local_authority().unwrap();
        std::fs::write(store.local_dir().join("git-setup.json"), b"prepared").unwrap();
        // Enter after the outer mode check, as a writer that waited for a
        // concurrent setup lock would. The under-lock check must still refuse.
        let result = super::Writer::acquire_after_mode_check(&store, false, false);
        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(store.ops_dir()).unwrap().count(), 0);
    }

    use super::*;
    use crate::op::{ScalarSet, make_create, make_note};
    use jiff::Timestamp;

    #[test]
    fn mode_transition_fences_a_writer_that_passed_its_first_check() {
        let root = tempfile::TempDir::new().unwrap();
        let store = Store::init(root.path()).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(store.local_dir().join("publication.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let other = store.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            other.require_local_authority().unwrap();
            tx.send(()).unwrap();
            Writer::acquire_after_mode_check(&other, false, false).is_err()
        });
        rx.recv().unwrap();
        // The transition is protected by the publication lock, exactly as a
        // future migration must install its incompatible mode marker.
        fs::write(store.root().join("remote.json"), b"{}").unwrap();
        drop(lock);
        assert!(writer.join().unwrap());
        assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 0);
        assert!(!directory(&store).exists());
    }

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
