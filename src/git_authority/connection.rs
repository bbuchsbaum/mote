//! Explicit empty-store setup, with durable identity and fail-closed recovery.
use super::{
    cache::{Cache, atomic_write},
    client::Client,
    history::{Binding, Snapshot},
    protocol::{MANIFEST_SCHEMA, Manifest, decode, encode, invalid, valid_oid},
    transport::GitTransport,
};
use crate::{
    MoteError, MoteResult,
    repo::{Format, Store},
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
    pub version: u32,
    pub remote: String,
    pub branch: String,
    pub store_id: String,
    pub generation: u64,
    pub root: String,
    pub capabilities: Vec<String>,
}
impl Descriptor {
    pub fn validate(&self) -> MoteResult<()> {
        if self.version != 1
            || !valid_oid(&self.root)
            || self.remote.is_empty()
            || !self
                .remote
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || self.branch.is_empty()
            || self.branch.starts_with('-')
            || self.branch.chars().any(char::is_control)
        {
            return invalid("invalid Git authority descriptor");
        }
        Manifest {
            schema: MANIFEST_SCHEMA.into(),
            store_id: self.store_id.clone(),
            generation: self.generation,
            ownership: "explicit-release-v1".into(),
            capabilities: self.capabilities.clone(),
            legacy_digest: None,
        }
        .validate()
    }
    pub fn binding(&self, endpoint_digest: String) -> Binding {
        Binding {
            version: 1,
            endpoint_digest,
            branch_ref: format!("refs/heads/{}", self.branch),
            store_id: self.store_id.clone(),
            generation: self.generation,
            root: self.root.clone(),
        }
    }
    pub fn check_snapshot(&self, snapshot: &Snapshot) -> MoteResult<()> {
        if self.root != snapshot.root
            || self.store_id != snapshot.manifest.store_id
            || self.generation != snapshot.manifest.generation
            || self.capabilities != snapshot.manifest.capabilities
        {
            return invalid("remote authority differs from selected project descriptor");
        }
        Ok(())
    }
    fn from_snapshot(transport: &GitTransport, snapshot: &Snapshot) -> Self {
        Self {
            version: 1,
            remote: transport.remote_name().into(),
            branch: transport
                .branch_ref()
                .trim_start_matches("refs/heads/")
                .into(),
            store_id: snapshot.manifest.store_id.clone(),
            generation: snapshot.manifest.generation,
            root: snapshot.root.clone(),
            capabilities: snapshot.manifest.capabilities.clone(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupOperation {
    Bootstrap,
    Connect,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupPhase {
    Prepared,
    RemoteConfirmed,
    Activated,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootArtifact {
    pub object_format: String,
    pub manifest_oid: String,
    pub manifest_bytes: Vec<u8>,
    pub tree_oid: String,
    pub tree_bytes: Vec<u8>,
    pub commit_oid: String,
    pub commit_bytes: Vec<u8>,
}
impl RootArtifact {
    pub fn validate(&self) -> MoteResult<()> {
        let len = match self.object_format.as_str() {
            "sha1" => 40,
            "sha256" => 64,
            _ => return invalid("unsupported root object format"),
        };
        if [&self.manifest_oid, &self.tree_oid, &self.commit_oid]
            .iter()
            .any(|oid| !valid_oid(oid) || oid.len() != len)
            || [&self.manifest_bytes, &self.tree_bytes, &self.commit_bytes]
                .iter()
                .any(|b| b.is_empty() || b.len() > 1 << 20)
        {
            return invalid("invalid retained authority root artifact");
        }
        let manifest: Manifest = decode(&self.manifest_bytes)?;
        manifest.validate()?;
        if encode(&manifest)? != self.manifest_bytes {
            return invalid("retained manifest is not canonical");
        }
        Ok(())
    }
}
fn required_option<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(d)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRecord {
    pub version: u32,
    pub operation: SetupOperation,
    pub original_format: Vec<u8>,
    pub original_format_digest: String,
    pub proposed_format: Vec<u8>,
    pub local_store_id: String,
    pub source_fingerprint: String,
    pub endpoint_digest: String,
    pub selected_remote: String,
    pub cache_root: PathBuf,
    pub descriptor: Descriptor,
    #[serde(deserialize_with = "required_option")]
    pub root_artifact: Option<RootArtifact>,
    #[serde(deserialize_with = "required_option")]
    pub confirmed_head: Option<String>,
    pub phase: SetupPhase,
}
impl SetupRecord {
    pub fn validate(&self) -> MoteResult<()> {
        self.descriptor.validate()?;
        let original: Format = decode(&self.original_format)?;
        if self.version != 1
            || original.store_id != self.local_store_id
            || !matches!(original.schema_version, 1 | 2)
            || self.original_format_digest != blake3::hash(&self.original_format).to_hex().as_str()
            || !digest_ok(&self.endpoint_digest)
            || !digest_ok(&self.source_fingerprint)
            || self.selected_remote.is_empty()
            || !self.cache_root.is_absolute()
            || self.proposed_format != proposed_format(&self.original_format, &self.descriptor)?
            || self
                .confirmed_head
                .as_deref()
                .is_some_and(|h| !valid_oid(h))
            || (self.phase != SetupPhase::Prepared && self.confirmed_head.is_none())
        {
            return invalid("invalid or inconsistent Git setup identity");
        }
        match (&self.operation, &self.root_artifact) {
            (SetupOperation::Bootstrap, Some(root)) => {
                root.validate()?;
                let manifest: Manifest = decode(&root.manifest_bytes)?;
                if original.schema_version != 1
                    || self.descriptor.store_id != original.store_id
                    || root.commit_oid != self.descriptor.root
                    || manifest.store_id != self.descriptor.store_id
                    || manifest.generation != self.descriptor.generation
                    || manifest.capabilities != self.descriptor.capabilities
                {
                    return invalid("bootstrap root differs from retained identity");
                }
            }
            (SetupOperation::Connect, None) if self.confirmed_head.is_some() => {}
            _ => return invalid("setup operation and retained root disagree"),
        }
        Ok(())
    }
}
fn digest_ok(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn proposed_format(original: &[u8], descriptor: &Descriptor) -> MoteResult<Vec<u8>> {
    let mut v: serde_json::Value = decode(original)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| MoteError::Invalid("FORMAT must be an object".into()))?;
    obj.insert("schema_version".into(), 2.into());
    obj.insert("store_id".into(), descriptor.store_id.clone().into());
    // Additional fence for legacy binaries that inspect authority.version but
    // predate the remote schema check. It does not qualify every old binary.
    obj.insert(
        "authority".into(),
        serde_json::json!({"version":u32::MAX,"genesis_hash":descriptor.root}),
    );
    encode(&v)
}
pub fn setup_path(store_root: &Path) -> PathBuf {
    store_root.join("local/git-setup.json")
}
pub fn descriptor_path(store_root: &Path) -> PathBuf {
    store_root.join("remote.json")
}
pub fn default_cache_root(store: &Store) -> PathBuf {
    store.local_dir().join("git-authority")
}
pub fn load_setup(store_root: &Path) -> MoteResult<Option<SetupRecord>> {
    let record: Option<SetupRecord> = read_canonical(&setup_path(store_root))?;
    if let Some(r) = &record {
        r.validate()?
    }
    Ok(record)
}
pub fn load_descriptor(store_root: &Path) -> MoteResult<Option<Descriptor>> {
    let descriptor: Option<Descriptor> = read_canonical(&descriptor_path(store_root))?;
    if let Some(d) = &descriptor {
        d.validate()?
    }
    Ok(descriptor)
}
fn read_canonical<T: serde::de::DeserializeOwned + Serialize>(
    path: &Path,
) -> MoteResult<Option<T>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let value: T = decode(&bytes)?;
    if encode(&value)? != bytes {
        return invalid("noncanonical Git setup metadata");
    }
    Ok(Some(value))
}
pub fn write_setup(store_root: &Path, record: &SetupRecord) -> MoteResult<()> {
    record.validate()?;
    if let Some(existing) = load_setup(store_root)? {
        let mut prior = existing;
        prior.phase = record.phase.clone();
        prior.confirmed_head = record.confirmed_head.clone();
        if prior != *record {
            return invalid("cannot replace retained Git setup identity");
        }
    }
    atomic_write(&setup_path(store_root), &encode(record)?)
}
fn write_portable(path: &Path, bytes: &[u8]) -> MoteResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MoteError::Invalid("metadata has no parent".into()))?;
    let temp = parent.join(format!(".git-setup-{}.tmp", ulid::Ulid::new()));
    crate::publish::write_tmp_durable(&temp, bytes)?;
    fs::rename(&temp, path)?;
    crate::publish::fsync_dir(parent)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MachineRoute {
    version: u32,
    remote: String,
    binding: Binding,
    capabilities: Vec<String>,
}
pub fn selected_remote(store: &Store, cache_root: &Path) -> MoteResult<String> {
    let descriptor = load_descriptor(store.root())?
        .ok_or_else(|| MoteError::Invalid("store has no Git descriptor".into()))?;
    let route: MachineRoute = read_canonical(&cache_root.join("route.json"))?.ok_or_else(|| {
        MoteError::Invalid("this machine is not connected; run authority connect".into())
    })?;
    if route.version != 1
        || route.binding != descriptor.binding(route.binding.endpoint_digest.clone())
        || route.capabilities != descriptor.capabilities
    {
        return invalid("machine routing differs from project identity");
    }
    Ok(route.remote)
}
pub fn open_client(
    store: &Store,
    transport: GitTransport,
    cache_root: &Path,
) -> MoteResult<Client> {
    checked_project_cache(store, &transport, cache_root)?;
    if setup_path(store.root()).exists() {
        return invalid("Git setup is incomplete; run authority resume");
    }
    let descriptor = load_descriptor(store.root())?
        .ok_or_else(|| MoteError::Invalid("store has no Git descriptor".into()))?;
    let route: MachineRoute = read_canonical(&cache_root.join("route.json"))?.ok_or_else(|| {
        MoteError::Invalid("this machine is not connected; run authority connect".into())
    })?;
    if route.version != 1
        || route.remote != transport.remote_name()
        || route.binding != descriptor.binding(transport.endpoint_digest())
        || route.capabilities != descriptor.capabilities
        || store.read_format()?.schema_version != 2
        || store.read_format()?.store_id != descriptor.store_id
    {
        return invalid("machine route or format differs from selected authority");
    }
    Client::open_bound(transport, cache_root, &route.binding, &route.capabilities)
}
fn checked_project_cache(
    store: &Store,
    transport: &GitTransport,
    path: &Path,
) -> MoteResult<PathBuf> {
    let resolved = transport.checked_cache_path(path)?;
    let local = store.local_dir().canonicalize()?;
    if resolved == local || !resolved.starts_with(local) {
        return invalid("project authority cache must have its own directory below store local/");
    }
    Ok(resolved)
}
fn source_fingerprint(store: &Store) -> MoteResult<String> {
    match fs::read_dir(store.ops_dir()) {
        Ok(mut files) => {
            if files.next().is_some() {
                return invalid("nonempty local history requires explicit migration");
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut evidence = Vec::<(String, String)>::new();
    if let Some(bytes) = read_bytes_optional(&crate::authority::binding_path(store))? {
        evidence.push((
            "local/authority.json".into(),
            blake3::hash(&bytes).to_hex().to_string(),
        ))
    }
    match fs::read_dir(crate::authority::directory(store)) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| MoteError::Invalid("invalid local authority path".into()))?;
                if name.len() != 25
                    || !name.ends_with(".json")
                    || !name[..20].bytes().all(|b| b.is_ascii_digit())
                    || !entry.file_type()?.is_file()
                {
                    return invalid(
                        "local publication or landing metadata requires recovery/migration before Git setup",
                    );
                }
                evidence.push((
                    format!("authority/{name}"),
                    blake3::hash(&fs::read(entry.path())?).to_hex().to_string(),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    evidence.sort();
    Ok(blake3::hash(&encode(&evidence)?).to_hex().to_string())
}
fn read_bytes_optional(path: &Path) -> MoteResult<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    IntentDurable,
    PushReturned,
    CacheConfirmed,
    RemoteConfirmed,
    MarkerDurable,
    FormatDurable,
    Activated,
}
pub fn bootstrap(
    store: &Store,
    transport: GitTransport,
    cache_root: &Path,
) -> MoteResult<Descriptor> {
    run_setup(
        store,
        transport,
        cache_root,
        SetupOperation::Bootstrap,
        &mut |_| Ok(()),
    )
}
pub fn connect(
    store: &Store,
    transport: GitTransport,
    cache_root: &Path,
) -> MoteResult<Descriptor> {
    run_setup(
        store,
        transport,
        cache_root,
        SetupOperation::Connect,
        &mut |_| Ok(()),
    )
}
pub fn resume(store: &Store, transport: GitTransport, cache_root: &Path) -> MoteResult<Descriptor> {
    let record = load_setup(store.root())?
        .ok_or_else(|| MoteError::Invalid("no retained Git setup to resume".into()))?;
    run_setup(store, transport, cache_root, record.operation, &mut |_| {
        Ok(())
    })
}
fn run_setup(
    store: &Store,
    transport: GitTransport,
    cache_root: &Path,
    operation: SetupOperation,
    hook: &mut dyn FnMut(Checkpoint) -> MoteResult<()>,
) -> MoteResult<Descriptor> {
    let canonical_store = Store::open(&fs::canonicalize(store.root())?)?;
    let store = &canonical_store;
    fs::create_dir_all(store.local_dir())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(store.local_dir().join("publication.lock"))?;
    lock.lock_exclusive()?;
    let cache = Cache::open(&checked_project_cache(store, &transport, cache_root)?)?;
    let cache_root = fs::canonicalize(cache.root())?;
    let mut record = if let Some(record) = load_setup(store.root())? {
        if record.operation != operation
            || record.endpoint_digest != transport.endpoint_digest()
            || record.selected_remote != transport.remote_name()
            || record
                .descriptor
                .binding(record.endpoint_digest.clone())
                .branch_ref
                != transport.branch_ref()
            || record.cache_root != cache_root
        {
            return invalid(
                "setup arguments differ from retained identity; use the original endpoint, branch and cache",
            );
        }
        record
    } else {
        let original = fs::read(store.format_path())?;
        let format: Format = decode(&original)?;
        let existing = load_descriptor(store.root())?;
        let source = source_fingerprint(store)?;
        if format.schema_version == 1 && existing.is_none() {
            store.require_local_authority()?;
            crate::authority::status(store)?;
        } else if format.schema_version != 2
            || existing
                .as_ref()
                .is_none_or(|d| d.store_id != format.store_id)
        {
            return invalid("local format and Git descriptor disagree");
        }
        let (descriptor, root_artifact, confirmed_head) = match operation {
            SetupOperation::Bootstrap => {
                if existing.is_some() || format.schema_version != 1 || cache.binding()?.is_some() {
                    return invalid("bootstrap requires an empty never-bound local store");
                }
                if transport.head()?.is_some() {
                    return invalid("authority branch already exists; use authority connect");
                }
                let manifest = Manifest::lifecycle(format.store_id.clone());
                let root = transport.capture_root(&manifest)?;
                let descriptor = Descriptor {
                    version: 1,
                    remote: transport.remote_name().into(),
                    branch: transport
                        .branch_ref()
                        .trim_start_matches("refs/heads/")
                        .into(),
                    store_id: manifest.store_id,
                    generation: manifest.generation,
                    root: root.commit_oid.clone(),
                    capabilities: manifest.capabilities,
                };
                (descriptor, Some(root), None)
            }
            SetupOperation::Connect => {
                let (_client, snapshot) = if let Some(d) = &existing {
                    Client::connect_expected(
                        transport.clone(),
                        &cache_root,
                        &d.binding(transport.endpoint_digest()),
                        &d.capabilities,
                        None,
                    )?
                } else {
                    Client::connect_snapshot(transport.clone(), &cache_root)?
                };
                let descriptor =
                    existing.unwrap_or_else(|| Descriptor::from_snapshot(&transport, &snapshot));
                descriptor.check_snapshot(&snapshot)?;
                (descriptor, None, Some(snapshot.head))
            }
        };
        let record = SetupRecord {
            version: 1,
            operation: operation.clone(),
            original_format_digest: blake3::hash(&original).to_hex().to_string(),
            proposed_format: proposed_format(&original, &descriptor)?,
            original_format: original,
            local_store_id: format.store_id,
            source_fingerprint: source,
            endpoint_digest: transport.endpoint_digest(),
            selected_remote: transport.remote_name().into(),
            cache_root: cache_root.clone(),
            descriptor,
            root_artifact,
            confirmed_head,
            phase: SetupPhase::Prepared,
        };
        write_setup(store.root(), &record)?;
        hook(Checkpoint::IntentDurable)?;
        record
    };
    record.validate()?;
    let current = fs::read(store.format_path())?;
    if current != record.original_format && current != record.proposed_format {
        return invalid("local FORMAT changed during Git setup");
    }
    if source_fingerprint(store)? != record.source_fingerprint {
        return invalid("local source changed during Git setup");
    }
    if let Some(descriptor) = load_descriptor(store.root())? {
        if descriptor != record.descriptor {
            return invalid("project descriptor changed during Git setup");
        }
    }
    if let Some(root) = &record.root_artifact {
        transport.import_root(root)?
    }
    if transport.head()?.is_none() {
        if record.operation != SetupOperation::Bootstrap
            || record.phase != SetupPhase::Prepared
            || record.confirmed_head.is_some()
            || cache.binding()?.is_some()
            || cache.previous_head()?.is_some()
        {
            return invalid("selected authority disappeared; it will not be recreated");
        }
        // Even denial/timeout can follow a previous ambiguous acceptance. The
        // selected remote is always observed again before declaring confirmation.
        let _attempt = transport.push(&record.descriptor.root)?;
        hook(Checkpoint::PushReturned)?;
    }
    let binding = record.descriptor.binding(record.endpoint_digest.clone());
    let (_client, snapshot) = Client::connect_expected(
        transport,
        &cache_root,
        &binding,
        &record.descriptor.capabilities,
        record.confirmed_head.as_deref(),
    )?;
    hook(Checkpoint::CacheConfirmed)?;
    record.descriptor.check_snapshot(&snapshot)?;
    record.confirmed_head = Some(snapshot.head);
    if record.phase == SetupPhase::Prepared {
        record.phase = SetupPhase::RemoteConfirmed
    }
    write_setup(store.root(), &record)?;
    hook(Checkpoint::RemoteConfirmed)?;
    write_portable(&descriptor_path(store.root()), &encode(&record.descriptor)?)?;
    hook(Checkpoint::MarkerDurable)?;
    write_portable(&store.format_path(), &record.proposed_format)?;
    hook(Checkpoint::FormatDurable)?;
    atomic_write(
        &cache_root.join("route.json"),
        &encode(&MachineRoute {
            version: 1,
            remote: record.selected_remote.clone(),
            binding,
            capabilities: record.descriptor.capabilities.clone(),
        })?,
    )?;
    record.phase = SetupPhase::Activated;
    write_setup(store.root(), &record)?;
    hook(Checkpoint::Activated)?;
    fs::remove_file(setup_path(store.root()))?;
    File::open(store.local_dir())?.sync_all()?;
    Ok(record.descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };
    use tempfile::{TempDir, tempdir};
    struct Fixture {
        td: TempDir,
    }
    fn git(path: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    impl Fixture {
        fn new() -> Self {
            let f = Self {
                td: tempdir().unwrap(),
            };
            git(
                f.td.path(),
                &[
                    "init",
                    "--bare",
                    "-q",
                    f.td.path().join("remote.git").to_str().unwrap(),
                ],
            );
            for name in ["a", "b"] {
                let repo = f.td.path().join(name);
                fs::create_dir(&repo).unwrap();
                git(&repo, &["init", "-q"]);
                git(
                    &repo,
                    &[
                        "remote",
                        "add",
                        "origin",
                        f.td.path().join("remote.git").to_str().unwrap(),
                    ],
                );
                Store::init(&repo).unwrap();
            }
            f
        }
        fn store(&self, name: &str) -> Store {
            Store::open(&self.td.path().join(name).join(".mote")).unwrap()
        }
        fn transport(&self, name: &str, scratch: &str) -> GitTransport {
            GitTransport::open(
                &self.td.path().join(name),
                "origin",
                "mote-state",
                &self.td.path().join(scratch),
            )
            .unwrap()
        }
        fn prepare(&self) -> SetupRecord {
            let store = self.store("a");
            let error = run_setup(
                &store,
                self.transport("a", "scratch"),
                &default_cache_root(&store),
                SetupOperation::Bootstrap,
                &mut |point| {
                    if point == Checkpoint::IntentDurable {
                        invalid("injected interruption")
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("injected interruption"));
            load_setup(store.root()).unwrap().unwrap()
        }
    }
    #[test]
    fn endpoint_and_source_changes_cannot_replace_a_retained_bootstrap() {
        let f = Fixture::new();
        let prior = f.prepare();
        let store = f.store("a");
        let other = f.td.path().join("other.git");
        git(
            f.td.path(),
            &["init", "--bare", "-q", other.to_str().unwrap()],
        );
        git(
            &f.td.path().join("a"),
            &["remote", "set-url", "origin", other.to_str().unwrap()],
        );
        assert!(
            resume(
                &store,
                f.transport("a", "retargeted"),
                &default_cache_root(&store)
            )
            .is_err()
        );
        assert_eq!(load_setup(store.root()).unwrap(), Some(prior.clone()));
        assert!(f.transport("a", "other-check").head().unwrap().is_none());
        git(
            &f.td.path().join("a"),
            &[
                "remote",
                "set-url",
                "origin",
                f.td.path().join("remote.git").to_str().unwrap(),
            ],
        );
        let mut changed = prior.original_format.clone();
        changed.push(b' ');
        fs::write(store.format_path(), &changed).unwrap();
        assert!(
            resume(
                &store,
                f.transport("a", "source-change"),
                &default_cache_root(&store)
            )
            .is_err()
        );
        assert_eq!(fs::read(store.format_path()).unwrap(), changed);
        assert_eq!(load_setup(store.root()).unwrap(), Some(prior));
        assert!(f.transport("a", "original-check").head().unwrap().is_none());
    }
    #[test]
    fn competing_bootstrap_conflicts_without_replacing_either_identity() {
        let f = Fixture::new();
        let prior = f.prepare();
        let b = f.store("b");
        let winner =
            bootstrap(&b, f.transport("b", "b-bootstrap"), &default_cache_root(&b)).unwrap();
        assert_ne!(winner.root, prior.descriptor.root);
        let a = f.store("a");
        assert!(resume(&a, f.transport("a", "a-resume"), &default_cache_root(&a)).is_err());
        assert_eq!(load_setup(a.root()).unwrap(), Some(prior));
        assert_eq!(
            f.transport("a", "head-check").head().unwrap(),
            Some(winner.root)
        );
        assert!(
            Cache::open(&default_cache_root(&a))
                .unwrap()
                .binding()
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn a_confirmed_missing_authority_is_never_recreated_by_setup_resume() {
        let f = Fixture::new();
        let a = f.store("a");
        assert!(
            run_setup(
                &a,
                f.transport("a", "prepare"),
                &default_cache_root(&a),
                SetupOperation::Bootstrap,
                &mut |point| if point == Checkpoint::RemoteConfirmed {
                    invalid("injected interruption")
                } else {
                    Ok(())
                }
            )
            .is_err()
        );
        let prior = load_setup(a.root()).unwrap().unwrap();
        assert_eq!(prior.phase, SetupPhase::RemoteConfirmed);
        git(
            f.td.path(),
            &[
                "--git-dir",
                f.td.path().join("remote.git").to_str().unwrap(),
                "update-ref",
                "-d",
                "refs/heads/mote-state",
            ],
        );
        assert!(resume(&a, f.transport("a", "resume"), &default_cache_root(&a)).is_err());
        assert!(f.transport("a", "check").head().unwrap().is_none());
        assert_eq!(load_setup(a.root()).unwrap(), Some(prior));
    }
    #[test]
    #[ignore = "subprocess helper, run only by the crash witness"]
    fn setup_crash_child() {
        let Some(root) = std::env::var_os("MOTE_TEST_SETUP_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let point = std::env::var("MOTE_TEST_SETUP_POINT").unwrap();
        let store = Store::open(&root.join("a/.mote")).unwrap();
        let transport = GitTransport::open(
            &root.join("a"),
            "origin",
            "mote-state",
            &root.join("scratch"),
        )
        .unwrap();
        run_setup(
            &store,
            transport,
            &default_cache_root(&store),
            SetupOperation::Bootstrap,
            &mut |checkpoint| {
                if format!("{checkpoint:?}") == point {
                    atomic_write(&root.join("checkpoint"), point.as_bytes())?;
                    loop {
                        thread::park();
                    }
                }
                Ok(())
            },
        )
        .unwrap();
        panic!("requested setup checkpoint was not reached");
    }
    fn die_at(f: &Fixture, point: &str) {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "git_authority::connection::tests::setup_crash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("MOTE_TEST_SETUP_ROOT", f.td.path())
            .env("MOTE_TEST_SETUP_POINT", point)
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_SESSION")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if f.td.path().join("checkpoint").is_file() {
                break;
            }
            if child.try_wait().unwrap().is_some() {
                let output = child.wait_with_output().unwrap();
                panic!(
                    "child exited before {point}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            if Instant::now() > deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("child failed to reach {point}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
    }
    #[test]
    fn actual_process_death_and_scratch_loss_preserve_setup_identity_at_each_boundary() {
        for point in [
            "IntentDurable",
            "PushReturned",
            "CacheConfirmed",
            "MarkerDurable",
            "FormatDurable",
            "Activated",
        ] {
            let f = Fixture::new();
            let a = f.store("a");
            die_at(&f, point);
            let before = load_setup(a.root()).unwrap().unwrap();
            let root = before.descriptor.root.clone();
            assert!(
                crate::authority::Writer::acquire(&a).is_err(),
                "intent must fence local writes at {point}"
            );
            // Every selected checkpoint follows completed Git children; the
            // killed helper cannot still own a process using this scratch.
            fs::remove_dir_all(f.td.path().join("scratch")).unwrap();
            let descriptor = resume(
                &a,
                f.transport("a", "replacement-scratch"),
                &default_cache_root(&a),
            )
            .unwrap();
            assert_eq!(descriptor.root, root, "root identity changed at {point}");
            assert_eq!(f.transport("a", "verify").head().unwrap(), Some(root));
            assert!(!setup_path(a.root()).exists());
            assert_eq!(a.read_format().unwrap().schema_version, 2);
            assert_eq!(fs::read_dir(a.ops_dir()).unwrap().count(), 0);
        }
    }
    #[test]
    fn death_after_cache_confirmation_cannot_recreate_deleted_authority() {
        let f = Fixture::new();
        let a = f.store("a");
        die_at(&f, "CacheConfirmed");
        let record = load_setup(a.root()).unwrap().unwrap();
        assert_eq!(record.phase, SetupPhase::Prepared);
        assert!(record.confirmed_head.is_none());
        let cache = Cache::open(&default_cache_root(&a)).unwrap();
        let before = cache.previous_head().unwrap();
        assert_eq!(before.as_deref(), Some(record.descriptor.root.as_str()));
        git(
            f.td.path(),
            &[
                "--git-dir",
                f.td.path().join("remote.git").to_str().unwrap(),
                "update-ref",
                "-d",
                "refs/heads/mote-state",
            ],
        );
        assert!(resume(&a, f.transport("a", "resume"), &default_cache_root(&a)).is_err());
        assert!(f.transport("a", "check").head().unwrap().is_none());
        assert_eq!(cache.previous_head().unwrap(), before);
        assert_eq!(load_setup(a.root()).unwrap(), Some(record));
    }
}
