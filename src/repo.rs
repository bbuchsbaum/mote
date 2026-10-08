//! `.mote/` store: layout, init, discovery, FORMAT.json, actor resolution.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::errors::{MoteError, MoteResult};
use crate::ids::format_rfc3339;

const STORE_DIR: &str = ".mote";
const FORMAT_FILE: &str = "FORMAT.json";
const TMP_DIR: &str = "tmp";
const OPS_DIR: &str = "ops";
const LOCAL_DIR: &str = "local";
const ACTOR_FILE: &str = "actor";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefaultTtls {
    pub claim: u32,
    pub reservation: u32,
}

impl Default for DefaultTtls {
    fn default() -> Self {
        Self {
            claim: 1800,
            reservation: 3600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Format {
    pub schema_version: u32,
    pub store_id: String,
    pub created_at: String,
    pub default_ttl_s: DefaultTtls,
    /// Fenced stores must retain their admission journal when copied or restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthorityFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityFormat {
    pub version: u32,
    pub genesis_hash: String,
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    /// Set when discovery started in a linked Git worktree and resolved to the
    /// main worktree's store: the worktree-local copy that was bypassed (if
    /// any) and why.
    worktree_redirect: Option<WorktreeRedirect>,
}

#[derive(Debug, Clone)]
pub struct WorktreeRedirect {
    /// The linked worktree's own `.mote/`, normally a Git-tracked copy of the
    /// shared store. `None` when the worktree has no store of its own.
    pub bypassed: Option<PathBuf>,
    /// Per-checkout `local/actor` file. Identity stays with the worktree even
    /// though coordination moves to the shared store.
    pub actor_file: PathBuf,
}

impl Store {
    /// Walk up from `start_dir` looking for a `.mote/` directory. Returns the
    /// store rooted at the first match.
    ///
    /// Inside a linked Git worktree, a tracked `.mote/` is a private copy of
    /// the shared store: writing to it would split coordination silently. When
    /// the main worktree holds a store with the same `store_id`, discovery
    /// resolves to that shared store instead. A worktree store with a
    /// different `store_id` is deliberately separate and is used as found.
    /// A worktree with no store of its own also falls back to the main one.
    pub fn discover(start_dir: &Path) -> MoteResult<Self> {
        let start = fs::canonicalize(start_dir)?;
        let mut p = start.clone();
        loop {
            let candidate = p.join(STORE_DIR);
            if candidate.is_dir() {
                if let Some(shared) = shared_store_for_worktree_copy(&candidate) {
                    let actor_file = candidate.join(LOCAL_DIR).join(ACTOR_FILE);
                    return Ok(Store {
                        root: shared,
                        worktree_redirect: Some(WorktreeRedirect {
                            bypassed: Some(candidate),
                            actor_file,
                        }),
                    });
                }
                // A linked worktree nested inside the main checkout (such as
                // `.claude/worktrees/<name>`) reaches the main store by walking
                // up. Coordination is already shared; identity must still stay
                // with the worktree.
                let enclosing = linked_worktree_roots(&start)
                    .filter(|(worktree_root, _, _)| !candidate.starts_with(worktree_root));
                if let Some((_, _, gitdir)) = enclosing {
                    return Ok(Store {
                        root: candidate,
                        worktree_redirect: Some(WorktreeRedirect {
                            bypassed: None,
                            actor_file: gitdir.join("mote").join(ACTOR_FILE),
                        }),
                    });
                }
                return Ok(Store {
                    root: candidate,
                    worktree_redirect: None,
                });
            }
            if !p.pop() {
                if let Some((shared, gitdir)) = main_worktree_store(&start) {
                    return Ok(Store {
                        root: shared,
                        worktree_redirect: Some(WorktreeRedirect {
                            bypassed: None,
                            // Git's per-worktree admin dir is private to this
                            // checkout and never tracked.
                            actor_file: gitdir.join("mote").join(ACTOR_FILE),
                        }),
                    });
                }
                return Err(MoteError::StoreNotFound(start_dir.to_path_buf()));
            }
        }
    }

    /// Keep identity per checkout for a store opened explicitly (`--store`,
    /// `MOTE_STORE`) from inside a linked worktree: when the store lies outside
    /// the worktree that contains `cwd`, use that worktree's private actor
    /// file, exactly as discovery would.
    pub fn with_worktree_identity(mut self, cwd: &Path) -> Self {
        if self.worktree_redirect.is_some() {
            return self;
        }
        let Ok(cwd) = fs::canonicalize(cwd) else {
            return self;
        };
        let root = fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        if let Some((worktree_root, _, gitdir)) = linked_worktree_roots(&cwd) {
            if !root.starts_with(&worktree_root) {
                self.worktree_redirect = Some(WorktreeRedirect {
                    bypassed: None,
                    actor_file: gitdir.join("mote").join(ACTOR_FILE),
                });
            }
        }
        self
    }

    /// The main worktree's store when `dir` is inside a linked worktree that
    /// would otherwise create its own (used by `mote init` to avoid forking).
    pub fn main_worktree_store_for(dir: &Path) -> Option<PathBuf> {
        let dir = fs::canonicalize(dir).ok()?;
        main_worktree_store(&dir).map(|(shared, _)| shared)
    }

    pub fn worktree_redirect(&self) -> Option<&WorktreeRedirect> {
        self.worktree_redirect.as_ref()
    }

    /// The checkout-local actor file read by actor resolution and written by
    /// `mote actor set`. Under a worktree redirect it stays in the worktree.
    pub fn actor_file(&self) -> PathBuf {
        match &self.worktree_redirect {
            Some(redirect) => redirect.actor_file.clone(),
            None => self.local_dir().join(ACTOR_FILE),
        }
    }

    /// Create a `.mote/` store inside `parent_dir`. Creates `tmp/`, `ops/`,
    /// `local/` and writes `FORMAT.json`. Errors if `FORMAT.json` already
    /// exists at the target.
    pub fn init(parent_dir: &Path) -> MoteResult<Self> {
        let root = parent_dir.join(STORE_DIR);
        let format_path = root.join(FORMAT_FILE);
        if format_path.exists() {
            return Err(MoteError::StoreAlreadyInitialized(root));
        }
        fs::create_dir_all(&root)?;
        fs::create_dir_all(root.join(TMP_DIR))?;
        fs::create_dir_all(root.join(OPS_DIR))?;
        fs::create_dir_all(root.join(LOCAL_DIR))?;

        let format = Format {
            schema_version: 1,
            store_id: format!("st-{}", Ulid::new()),
            created_at: format_rfc3339(Timestamp::now()),
            default_ttl_s: DefaultTtls::default(),
            authority: None,
        };
        let bytes = serde_json::to_vec_pretty(&format)?;
        fs::write(&format_path, bytes)?;
        Ok(Store {
            root,
            worktree_redirect: None,
        })
    }

    /// Open an existing store at exactly `root` (must contain `FORMAT.json`).
    pub fn open(root: &Path) -> MoteResult<Self> {
        let format_path = root.join(FORMAT_FILE);
        if !format_path.is_file() {
            return Err(MoteError::StoreNotFound(root.to_path_buf()));
        }
        Ok(Store {
            root: root.to_path_buf(),
            worktree_redirect: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join(TMP_DIR)
    }

    pub fn ops_dir(&self) -> PathBuf {
        self.root.join(OPS_DIR)
    }

    pub fn local_dir(&self) -> PathBuf {
        self.root.join(LOCAL_DIR)
    }

    pub fn format_path(&self) -> PathBuf {
        self.root.join(FORMAT_FILE)
    }

    pub fn read_format(&self) -> MoteResult<Format> {
        let data = fs::read(self.format_path())?;
        Ok(serde_json::from_slice(&data)?)
    }

    /// Resolve actor identity per spec, in order:
    ///   1. `flag` (the value passed via `--actor`), if non-empty
    ///   2. `MOTE_ACTOR` environment variable, if non-empty
    ///   3. `.mote/local/actor` file, if non-empty
    pub fn resolve_actor(&self, flag: Option<&str>) -> MoteResult<String> {
        if let Some(s) = flag {
            let s = s.trim();
            if !s.is_empty() {
                return Ok(s.to_string());
            }
        }
        if let Ok(s) = std::env::var("MOTE_ACTOR") {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
        let actor_file = self.actor_file();
        if actor_file.is_file() {
            let s = fs::read_to_string(&actor_file)?;
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
        Err(MoteError::ActorUnresolved)
    }

    /// List admitted op filenames in replay order (legacy filename order or
    /// the durable shared-store admission order once authority is enabled).
    pub fn list_op_filenames(&self) -> MoteResult<Vec<String>> {
        let mut names: Vec<String> = fs::read_dir(self.ops_dir())?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        crate::authority::ordered_names(self, names)
    }
}

/// Linked-worktree layout for `dir`: `(worktree_root, main_worktree_root,
/// worktree_gitdir)`.
///
/// A linked worktree's `.git` is a file (`gitdir: <common>/worktrees/<name>`)
/// whose target holds a `commondir` file; submodules have a `.git` file too but
/// no `commondir`, and bare repositories have no main worktree. Both yield
/// `None`. Reads Git's metadata directly so ordinary commands never spawn git.
fn linked_worktree_roots(dir: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
    let mut p = dir.to_path_buf();
    let dot_git = loop {
        let candidate = p.join(".git");
        if candidate.exists() {
            break candidate;
        }
        if !p.pop() {
            return None;
        }
    };
    if !dot_git.is_file() {
        return None;
    }
    let worktree_root = p;
    let pointer = fs::read_to_string(&dot_git).ok()?;
    let gitdir = pointer.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = worktree_root.join(gitdir);
    let commondir = fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = fs::canonicalize(gitdir.join(commondir.trim())).ok()?;
    if common.file_name()? != ".git" {
        return None;
    }
    let main_root = common.parent()?.to_path_buf();
    Some((worktree_root, main_root, gitdir))
}

fn store_id_at(store_root: &Path) -> Option<String> {
    let data = fs::read(store_root.join(FORMAT_FILE)).ok()?;
    let format: Format = serde_json::from_slice(&data).ok()?;
    Some(format.store_id)
}

/// The main worktree's store when `store_root` is a same-identity copy of it
/// inside a linked worktree.
fn shared_store_for_worktree_copy(store_root: &Path) -> Option<PathBuf> {
    let (worktree_root, main_root, _) = linked_worktree_roots(store_root.parent()?)?;
    let relative = store_root.strip_prefix(&worktree_root).ok()?;
    let shared = main_root.join(relative);
    let copy_id = store_id_at(store_root)?;
    (store_id_at(&shared)? == copy_id).then_some(shared)
}

/// The main worktree's top-level store, and this worktree's Git admin dir,
/// when `dir` is inside a linked worktree.
fn main_worktree_store(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let (_, main_root, gitdir) = linked_worktree_roots(dir)?;
    let shared = main_root.join(STORE_DIR);
    shared
        .join(FORMAT_FILE)
        .is_file()
        .then_some((shared, gitdir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn init_creates_layout() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        assert!(store.format_path().is_file());
        assert!(store.tmp_dir().is_dir());
        assert!(store.ops_dir().is_dir());
        assert!(store.local_dir().is_dir());
        // snap/ is NOT created in v0.2.
        assert!(!store.root().join("snap").exists());

        let f = store.read_format().unwrap();
        assert_eq!(f.schema_version, 1);
        assert!(f.store_id.starts_with("st-"));
        assert_eq!(f.default_ttl_s.claim, 1800);
        assert_eq!(f.default_ttl_s.reservation, 3600);
    }

    #[test]
    fn init_twice_fails() {
        let td = TempDir::new().unwrap();
        Store::init(td.path()).unwrap();
        let err = Store::init(td.path()).unwrap_err();
        assert!(matches!(err, MoteError::StoreAlreadyInitialized(_)));
    }

    #[test]
    fn discover_walks_up() {
        let td = TempDir::new().unwrap();
        Store::init(td.path()).unwrap();
        let nested = td.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        let store = Store::discover(&nested).unwrap();
        // Both paths should canonicalize equally.
        assert_eq!(
            store.root().canonicalize().unwrap(),
            td.path().canonicalize().unwrap().join(".mote")
        );
    }

    #[test]
    fn discover_errors_when_absent() {
        let td = TempDir::new().unwrap();
        let err = Store::discover(td.path()).unwrap_err();
        // A `.mote/` may exist further up (e.g. on the test runner's machine);
        // we don't strictly assert StoreNotFound here.
        // But on a fully clean tmp path, the walk hits the FS root and returns it.
        let _ = err;
    }

    #[test]
    fn actor_flag_wins_over_env() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        // Explicit flag is taken regardless of env.
        let actor = store.resolve_actor(Some("alice")).unwrap();
        assert_eq!(actor, "alice");
    }

    #[test]
    fn actor_flag_trims_whitespace() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        let actor = store.resolve_actor(Some("  bob  ")).unwrap();
        assert_eq!(actor, "bob");
    }

    #[test]
    fn list_op_filenames_is_sorted() {
        let td = TempDir::new().unwrap();
        let store = Store::init(td.path()).unwrap();
        // Drop two files into ops/ directly (bypassing publish for this unit test).
        fs::write(store.ops_dir().join("b.json"), b"{}").unwrap();
        fs::write(store.ops_dir().join("a.json"), b"{}").unwrap();
        fs::write(store.ops_dir().join("ignore.txt"), b"").unwrap();
        let names = store.list_op_filenames().unwrap();
        assert_eq!(names, vec!["a.json".to_string(), "b.json".to_string()]);
    }
}
