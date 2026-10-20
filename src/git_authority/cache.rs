//! Durable local identity and derived cache for a remote authority.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::history::{Binding, Snapshot};
use super::protocol::{decode, encode, invalid, valid_oid};
use crate::{MoteError, MoteResult};

pub struct Cache {
    root: PathBuf,
}

pub struct CacheLock {
    _file: File,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedView {
    pub version: u32,
    pub binding: Binding,
    pub head: String,
    pub sequence: u64,
    pub observed_at: String,
    pub fresh: bool,
    pub values: serde_json::Value,
}

impl Cache {
    pub fn open(root: &Path) -> MoteResult<Self> {
        let root = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()?.join(root)
        };
        durable_directory(&root)?;
        private_directory(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Callers retain this lock across read/modify/write or network submission.
    pub fn lock(&self) -> MoteResult<CacheLock> {
        let file = private_options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("client.lock"))?;
        file.lock_exclusive()?;
        Ok(CacheLock { _file: file })
    }

    pub fn binding(&self) -> MoteResult<Option<Binding>> {
        read_optional(&self.root.join("binding.json"))
    }

    pub fn bind(&self, binding: &Binding) -> MoteResult<()> {
        if !valid_oid(&binding.root) || binding.version != 1 || binding.generation == 0 {
            return invalid("invalid authority binding");
        }
        if let Some(existing) = self.binding()? {
            if existing != *binding {
                return invalid("cache is already bound to a different authority");
            }
            return Ok(());
        }
        atomic_write(&self.root.join("binding.json"), &encode(binding)?)
    }

    pub fn previous_head(&self) -> MoteResult<Option<String>> {
        let head: Option<String> = read_optional(&self.root.join("confirmed-head.json"))?;
        if head.as_deref().is_some_and(|h| !valid_oid(h)) {
            return invalid("invalid cached high-water mark");
        }
        Ok(head)
    }

    pub fn observe(&self, snapshot: &Snapshot) -> MoteResult<()> {
        let binding = self
            .binding()?
            .ok_or_else(|| MoteError::Invalid("cache is not bound".into()))?;
        if snapshot.root != binding.root
            || snapshot.manifest.store_id != binding.store_id
            || snapshot.manifest.generation != binding.generation
        {
            return invalid("snapshot does not match cache binding");
        }
        if self.previous_head()?.is_some_and(|head| {
            head != snapshot.root
                && head != snapshot.head
                && !snapshot
                    .receipts
                    .values()
                    .any(|receipt| receipt.commit == head)
        }) {
            return invalid("snapshot would lower the confirmed authority high-water mark");
        }
        // Preserve the high-water mark even if writing the optional view fails.
        atomic_write(
            &self.root.join("confirmed-head.json"),
            &encode(&snapshot.head)?,
        )?;
        let view = CachedView {
            version: 1,
            binding,
            head: snapshot.head.clone(),
            sequence: snapshot.state.sequence,
            observed_at: crate::ids::format_rfc3339(jiff::Timestamp::now()),
            fresh: false,
            values: serde_json::to_value(&snapshot.state.values)?,
        };
        atomic_write(&self.root.join("view.json"), &encode(&view)?)
    }

    /// This is explicitly a derived, possibly stale view. Mutation planning
    /// never uses it; a fresh chain must be fetched and validated instead.
    pub fn cached_view(&self) -> MoteResult<Option<CachedView>> {
        let mut view: Option<CachedView> = read_optional(&self.root.join("view.json"))?;
        if let Some(view) = &mut view {
            if view.version != 1
                || Some(&view.binding) != self.binding()?.as_ref()
                || !valid_oid(&view.head)
            {
                return invalid("cached view is incompatible with authority binding");
            }
            view.fresh = false;
        }
        Ok(view)
    }
}

pub(crate) fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> MoteResult<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(decode(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> MoteResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| MoteError::Invalid("missing local state directory".into()))?;
    durable_directory(parent)?;
    private_directory(parent)?;
    let temporary = parent.join(format!(
        ".mote-{}-{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| -> MoteResult<()> {
        let mut file = private_options()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn private_directory(path: &Path) -> MoteResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// Make every newly reachable directory durable before a binding or request
// journal can be reported persisted. Existing ancestors are synced as well:
// they may have been created by an interrupted earlier attempt or the caller.
fn durable_directory(path: &Path) -> MoteResult<()> {
    durable_directory_with(path, &mut |directory| {
        File::open(directory)?.sync_all()?;
        Ok(())
    })
}

fn durable_directory_with(
    path: &Path,
    sync: &mut impl FnMut(&Path) -> MoteResult<()>,
) -> MoteResult<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if parent != path && !parent.as_os_str().is_empty() {
            durable_directory_with(parent, sync)?;
        }
    }
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => (),
        Err(error) => return Err(error.into()),
    }
    sync(path)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            sync(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_request_directory_sync_failure_stops_before_journal_publication() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().join("new-cache");
        durable_directory(&root).unwrap();
        let requests = root.join("requests");
        let mut failed = false;
        let result = durable_directory_with(&requests, &mut |directory| {
            if directory == root && requests.exists() {
                failed = true;
                return Err(std::io::Error::other("injected parent-directory sync failure").into());
            }
            Ok(())
        });
        assert!(failed && result.is_err());
        assert!(!requests.join("first.json").exists());
        // Retrying a directory left by that failed attempt still performs the
        // parent sync, rather than trusting existence as durability evidence.
        let mut linked = false;
        durable_directory_with(&requests, &mut |directory| {
            if directory == root && requests.exists() {
                linked = true;
            }
            Ok(())
        })
        .unwrap();
        assert!(linked);
        atomic_write(&requests.join("first.json"), b"{}").unwrap();
        assert_eq!(fs::read(requests.join("first.json")).unwrap(), b"{}");
    }
}
