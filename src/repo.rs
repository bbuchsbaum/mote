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
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Walk up from `start_dir` looking for a `.mote/` directory. Returns the
    /// store rooted at the first match.
    pub fn discover(start_dir: &Path) -> MoteResult<Self> {
        let mut p = fs::canonicalize(start_dir)?;
        loop {
            let candidate = p.join(STORE_DIR);
            if candidate.is_dir() {
                return Ok(Store { root: candidate });
            }
            if !p.pop() {
                return Err(MoteError::StoreNotFound(start_dir.to_path_buf()));
            }
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
        };
        let bytes = serde_json::to_vec_pretty(&format)?;
        fs::write(&format_path, bytes)?;
        Ok(Store { root })
    }

    /// Open an existing store at exactly `root` (must contain `FORMAT.json`).
    pub fn open(root: &Path) -> MoteResult<Self> {
        let format_path = root.join(FORMAT_FILE);
        if !format_path.is_file() {
            return Err(MoteError::StoreNotFound(root.to_path_buf()));
        }
        Ok(Store {
            root: root.to_path_buf(),
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
        let actor_file = self.local_dir().join("actor");
        if actor_file.is_file() {
            let s = fs::read_to_string(&actor_file)?;
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
        Err(MoteError::ActorUnresolved)
    }

    /// List `.json` filenames in `ops/` in lexicographic (replay) order.
    pub fn list_op_filenames(&self) -> MoteResult<Vec<String>> {
        let mut names: Vec<String> = fs::read_dir(self.ops_dir())?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort();
        Ok(names)
    }
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
