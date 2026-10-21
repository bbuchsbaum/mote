//! Private complete-store custody before migration. No credentials or machine
//! files are exported into the shared authority archive.
use super::protocol::{decode, encode, invalid};
use crate::{MoteError, MoteResult, Store};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileRecord {
    pub digest: String,
    pub bytes: u64,
    pub mode: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub schema: String,
    pub source: PathBuf,
    pub store_id: String,
    pub format_digest: String,
    pub directories: BTreeMap<String, u32>,
    pub files: BTreeMap<String, FileRecord>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Custody {
    pub path: PathBuf,
    pub digest: String,
}
fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}
fn inventory_digest(inventory: &Inventory) -> MoteResult<String> {
    let mut h = blake3::Hasher::new();
    h.update(b"mote.legacy-backup.v1\0");
    h.update(&encode(inventory)?);
    Ok(h.finalize().to_hex().to_string())
}
#[cfg(unix)]
fn mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}
#[cfg(not(unix))]
fn mode(metadata: &fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o400
    } else {
        0o600
    }
}
pub(crate) fn private_directory(path: &Path) -> MoteResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path)?;
    Ok(())
}
fn set_file_mode(path: &Path, value: u32) -> MoteResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(value))?;
    }
    #[cfg(not(unix))]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_readonly(value & 0o200 == 0);
        fs::set_permissions(path, permissions)?;
    }
    File::open(path)?.sync_all()?;
    Ok(())
}
fn walk(
    root: &Path,
    relative: &str,
    directories: &mut BTreeMap<String, u32>,
    files: &mut BTreeMap<String, FileRecord>,
    copy_to: Option<&Path>,
) -> MoteResult<()> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_dir() {
        return invalid("backup directory is not a regular directory");
    }
    directories.insert(relative.into(), mode(&metadata));
    if let Some(destination) = copy_to {
        private_directory(&destination.join(relative))?;
    }
    let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().into_string().map_err(|_| {
            MoteError::Invalid("non-UTF8 store path prevents complete migration backup".into())
        })?;
        if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
            return invalid("unsupported store path prevents complete migration backup");
        }
        let name = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_dir() {
            walk(root, &name, directories, files, copy_to)?;
        } else if metadata.file_type().is_file() {
            let bytes = fs::read(entry.path())?;
            let record = FileRecord {
                digest: hash(&bytes),
                bytes: bytes.len() as u64,
                mode: mode(&metadata),
            };
            if let Some(destination) = copy_to {
                let output = destination.join(&name);
                crate::publish::write_tmp_durable(&output, &bytes)?;
                set_file_mode(&output, record.mode)?;
            }
            files.insert(name, record);
        } else {
            return invalid("symlink or nonregular store entry prevents complete migration backup");
        }
    }
    Ok(())
}

fn source_inventory(store: &Store, copy_to: Option<&Path>) -> MoteResult<Inventory> {
    let source = store.root().canonicalize()?;
    let mut inventory = Inventory {
        schema: "mote.legacy-backup.v1".into(),
        source,
        store_id: store.read_format()?.store_id,
        format_digest: hash(&fs::read(store.format_path())?),
        directories: BTreeMap::new(),
        files: BTreeMap::new(),
    };
    walk(
        &inventory.source,
        "",
        &mut inventory.directories,
        &mut inventory.files,
        copy_to,
    )?;
    Ok(inventory)
}

/// Caller must own the selected source's publication lock. The destination is
/// new and outside the selected project's tree. Partial output is retained on
/// failure and can never be mistaken for a complete backup without its receipt.
pub(crate) fn create(store: &Store, destination: &Path) -> MoteResult<Custody> {
    let parent = destination
        .parent()
        .ok_or_else(|| MoteError::Invalid("backup needs a parent".into()))?
        .canonicalize()?;
    let leaf = destination
        .file_name()
        .ok_or_else(|| MoteError::Invalid("backup needs a new directory name".into()))?;
    let destination = parent.join(leaf);
    let project = store
        .root()
        .canonicalize()?
        .parent()
        .ok_or_else(|| MoteError::Invalid("store needs a project parent".into()))?
        .to_path_buf();
    if destination.starts_with(&project) || project.starts_with(&destination) {
        return invalid("private migration backup must be outside the selected project tree");
    }
    private_directory(&destination)?;
    let inventory = source_inventory(store, Some(&destination.join("store")))?;
    if source_inventory(store, None)? != inventory {
        return invalid("selected source changed while copying its migration backup");
    }
    let mut directories: Vec<_> = inventory.directories.keys().collect();
    directories.sort_by_key(|name| std::cmp::Reverse(name.split('/').count()));
    for name in directories {
        crate::publish::fsync_dir(&destination.join("store").join(name))?;
    }
    crate::publish::write_tmp_durable(&destination.join("inventory.json"), &encode(&inventory)?)?;
    crate::publish::fsync_dir(&destination)?;
    crate::publish::fsync_dir(&parent)?;
    let custody = Custody {
        path: destination,
        digest: inventory_digest(&inventory)?,
    };
    verify(&custody)?;
    Ok(custody)
}

pub fn verify(custody: &Custody) -> MoteResult<Inventory> {
    if !custody.path.is_absolute() || !fs::symlink_metadata(&custody.path)?.file_type().is_dir() {
        return invalid("invalid migration backup location");
    }
    #[cfg(unix)]
    if mode(&fs::metadata(&custody.path)?) & 0o077 != 0 {
        return invalid("migration backup is not private");
    }
    let top = fs::read_dir(&custody.path)?
        .map(|e| Ok(e?.file_name()))
        .collect::<std::io::Result<BTreeSet<_>>>()?;
    if top != BTreeSet::from(["store".into(), "inventory.json".into()]) {
        return invalid("migration backup has unexpected or missing top-level entries");
    }
    let receipt = custody.path.join("inventory.json");
    if !fs::symlink_metadata(&receipt)?.file_type().is_file() {
        return invalid("migration backup inventory must be a regular file");
    }
    let bytes = fs::read(receipt)?;
    let inventory: Inventory = decode(&bytes)?;
    if inventory.schema != "mote.legacy-backup.v1"
        || !inventory.source.is_absolute()
        || encode(&inventory)? != bytes
        || inventory_digest(&inventory)? != custody.digest
    {
        return invalid("migration backup inventory or custody digest differs");
    }
    let mut directories = BTreeMap::new();
    let mut files = BTreeMap::new();
    walk(
        &custody.path.join("store"),
        "",
        &mut directories,
        &mut files,
        None,
    )?;
    // Backup directories stay private rather than copying source directory
    // permissions; original permissions are retained in the inventory.
    if directories.keys().collect::<Vec<_>>() != inventory.directories.keys().collect::<Vec<_>>()
        || files != inventory.files
        || files
            .get("FORMAT.json")
            .is_none_or(|f| f.digest != inventory.format_digest)
    {
        return invalid("complete migration backup bytes, modes or inventory differ");
    }
    let format: crate::repo::Format = decode(&fs::read(custody.path.join("store/FORMAT.json"))?)?;
    if format.schema_version != 1 || format.store_id != inventory.store_id {
        return invalid("migration backup does not contain the selected local store");
    }
    Ok(inventory)
}

fn copy_link(bytes: &[u8], mode: u32, temporary: &Path, destination: &Path) -> MoteResult<()> {
    if let Ok(metadata) = fs::symlink_metadata(destination) {
        if !metadata.file_type().is_file() || fs::read(destination)? != bytes {
            return invalid("rollback output contains a changed or unexpected file");
        }
        return set_file_mode(destination, mode);
    }
    if temporary.exists() {
        if !fs::symlink_metadata(temporary)?.file_type().is_file() {
            return invalid("rollback staging file is not regular");
        }
        if fs::read(temporary)? != bytes {
            // This exact staging file is private, owned by the pinned restore
            // journal and regenerable from the verified complete backup.
            fs::remove_file(temporary)?;
        }
    }
    if !temporary.exists() {
        crate::publish::write_tmp_durable(temporary, bytes)?;
    }
    set_file_mode(temporary, mode)?;
    fs::hard_link(temporary, destination)?;
    crate::publish::fsync_dir(destination.parent().expect("copy path has parent"))
}

/// The caller has exclusively established and pinned both private directories.
/// FORMAT remains absent, so this cannot expose a partly restored Mote store.
pub(crate) fn copy_without_format(
    custody: &Custody,
    destination: &Path,
    working: &Path,
    hook: &mut dyn FnMut() -> MoteResult<()>,
) -> MoteResult<Inventory> {
    let inventory = verify(custody)?;
    if destination.join("FORMAT.json").exists() {
        return invalid("rollback FORMAT may already be visible; copying is forbidden");
    }
    for relative in inventory.directories.keys().filter(|p| !p.is_empty()) {
        let path = destination.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => return invalid("rollback output directory is not regular"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => private_directory(&path)?,
            Err(e) => return Err(e.into()),
        }
    }
    for (index, (relative, record)) in inventory.files.iter().enumerate() {
        if relative == "FORMAT.json" {
            continue;
        }
        let bytes = fs::read(custody.path.join("store").join(relative))?;
        if hash(&bytes) != record.digest || bytes.len() as u64 != record.bytes {
            return invalid("backup changed while restoring a file");
        }
        copy_link(
            &bytes,
            record.mode,
            &working.join(format!("{index:020}.bin")),
            &destination.join(relative),
        )?;
        hook()?;
    }
    let mut directories: Vec<_> = inventory.directories.iter().collect();
    directories.sort_by_key(|(p, _)| std::cmp::Reverse(p.split('/').count()));
    for (name, original_mode) in directories {
        let path = destination.join(name);
        // Preserve a private root even when the original store was public.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(if name.is_empty() {
                    0o700
                } else {
                    *original_mode
                }),
            )?;
        }
        #[cfg(not(unix))]
        let _ = original_mode;
        crate::publish::fsync_dir(&path)?;
    }
    crate::publish::fsync_dir(working)?;
    Ok(inventory)
}

pub(crate) fn verify_unpublished_copy(inventory: &Inventory, destination: &Path) -> MoteResult<()> {
    let mut directories = BTreeMap::new();
    let mut files = BTreeMap::new();
    walk(destination, "", &mut directories, &mut files, None)?;
    let mut expected_files = inventory.files.clone();
    expected_files.remove("FORMAT.json");
    let mut expected_directories = inventory.directories.clone();
    expected_directories.insert("".into(), 0o700);
    if files != expected_files || directories != expected_directories {
        return invalid(
            "rollback copy bytes, modes or complete inventory differ before publication",
        );
    }
    Ok(())
}

/// Prepare complete FORMAT bytes on the same filesystem as the destination.
/// Linking them later is atomic and refuses an existing destination FORMAT.
pub(crate) fn prepare_format(custody: &Custody, working: &Path) -> MoteResult<PathBuf> {
    let inventory = verify(custody)?;
    let record = inventory
        .files
        .get("FORMAT.json")
        .ok_or_else(|| MoteError::Invalid("backup has no FORMAT".into()))?;
    let bytes = fs::read(custody.path.join("store/FORMAT.json"))?;
    let path = working.join("FORMAT.complete");
    if path.exists() {
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            return invalid("rollback FORMAT staging is not a regular file");
        }
        if fs::read(&path)? != bytes {
            // Called only while positively prepublication, in the pinned
            // private working directory; this file is verified-regenerable.
            fs::remove_file(&path)?;
        }
    }
    if !path.exists() {
        crate::publish::write_tmp_durable(&path, &bytes)?;
    }
    set_file_mode(&path, record.mode)?;
    crate::publish::fsync_dir(working)?;
    Ok(path)
}

pub(crate) fn verify_prepared_format(
    inventory: &Inventory,
    working: &Path,
    expected: &[u8],
) -> MoteResult<PathBuf> {
    let record = inventory
        .files
        .get("FORMAT.json")
        .ok_or_else(|| MoteError::Invalid("backup has no FORMAT".into()))?;
    let path = working.join("FORMAT.complete");
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file()
        || mode(&metadata) != record.mode
        || fs::read(&path)? != expected
        || hash(expected) != record.digest
    {
        return invalid("authorized rollback FORMAT bytes or mode differ");
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_private_backup_preserves_unshared_bytes_and_detects_damage() {
        let td = tempfile::tempdir().unwrap();
        let project = td.path().join("project");
        fs::create_dir(&project).unwrap();
        let store = Store::init(&project).unwrap();
        fs::create_dir(store.local_dir().join("empty")).unwrap();
        fs::write(store.local_dir().join("actor"), b"private fixture actor\0").unwrap();
        fs::write(store.local_dir().join("publication.lock"), b"").unwrap();
        let before = source_inventory(&store, None).unwrap();
        let custody = create(&store, &td.path().join("backup")).unwrap();
        assert_eq!(verify(&custody).unwrap(), before);
        assert_eq!(source_inventory(&store, None).unwrap(), before);
        assert!(create(&store, &custody.path).is_err());
        fs::write(custody.path.join("store/local/actor"), b"changed").unwrap();
        assert!(verify(&custody).is_err());
    }
    #[test]
    fn backup_refuses_inside_project_symlinks_extra_files_and_nonprivate_custody() {
        let td = tempfile::tempdir().unwrap();
        let project = td.path().join("project");
        fs::create_dir(&project).unwrap();
        let store = Store::init(&project).unwrap();
        assert!(create(&store, &project.join("backup")).is_err());
        let custody = create(&store, &td.path().join("good")).unwrap();
        fs::write(custody.path.join("store/untracked"), b"extra").unwrap();
        assert!(verify(&custody).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::{fs::PermissionsExt, fs::symlink};
            symlink("missing", store.local_dir().join("link")).unwrap();
            assert!(create(&store, &td.path().join("symlink")).is_err());
            fs::remove_file(store.local_dir().join("link")).unwrap();
            let custody = create(&store, &td.path().join("private")).unwrap();
            fs::set_permissions(&custody.path, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(verify(&custody).is_err());
        }
    }
}
