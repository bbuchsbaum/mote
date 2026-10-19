//! `mote fsck`: validate filenames, JSON, and content hashes; sweep stale
//! entries from `tmp/` when asked. Out of scope (and intentionally so):
//! lock healing, any heuristic deletion in `ops/`, snapshot rebuilds.

use std::fs;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::canonical;
use crate::errors::MoteResult;
use crate::ids;
use crate::op::Op;
use crate::repo::Store;

#[derive(Debug, Default)]
pub struct FsckReport {
    pub ops_checked: usize,
    pub bad_filename: Vec<String>,
    pub bad_json: Vec<String>,
    /// (filename, expected hash from filename, recomputed hash from content)
    pub bad_hash: Vec<(String, String, String)>,
    /// (filename, deserialization error). The bytes parsed as JSON but did not
    /// match a known op kind / shape.
    pub bad_op_shape: Vec<(String, String)>,
    pub tmp_total: usize,
    pub tmp_cleaned: usize,
}

impl FsckReport {
    pub fn is_clean(&self) -> bool {
        self.bad_filename.is_empty()
            && self.bad_json.is_empty()
            && self.bad_hash.is_empty()
            && self.bad_op_shape.is_empty()
    }
}

pub fn run(store: &Store, clean_tmp: bool) -> MoteResult<FsckReport> {
    let mut report = FsckReport::default();
    if crate::authority::is_bound(store)? || crate::authority::enabled(store) {
        // The admission order and bound op bytes are source of truth too.
        store.list_op_filenames()?;
    }

    for entry in fs::read_dir(store.ops_dir())? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !name.ends_with(".json") {
            continue;
        }
        report.ops_checked += 1;
        let bytes = fs::read(entry.path())?;
        for defect in check_op(&name, &bytes) {
            match defect {
                OpDefect::Filename => report.bad_filename.push(name.clone()),
                OpDefect::Json => report.bad_json.push(name.clone()),
                OpDefect::Hash { expected, got } => {
                    report.bad_hash.push((name.clone(), expected, got))
                }
                OpDefect::Shape(error) => report.bad_op_shape.push((name.clone(), error)),
            }
        }
    }

    // tmp/ pass.
    let tmp_dir = store.tmp_dir();
    if clean_tmp {
        let now = SystemTime::now();
        let threshold = Duration::from_secs(3600);
        for entry in fs::read_dir(&tmp_dir)? {
            let entry = entry?;
            report.tmp_total += 1;
            let path = entry.path();
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age > threshold && fs::remove_file(&path).is_ok() {
                report.tmp_cleaned += 1;
            }
        }
    } else {
        report.tmp_total = fs::read_dir(&tmp_dir)?.count();
    }

    Ok(report)
}

/// One way an op file fails validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpDefect {
    Filename,
    Json,
    /// (expected hash from filename, recomputed hash from content)
    Hash {
        expected: String,
        got: String,
    },
    Shape(String),
}

impl std::fmt::Display for OpDefect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpDefect::Filename => write!(f, "bad filename"),
            OpDefect::Json => write!(f, "bad json"),
            OpDefect::Hash { expected, got } => {
                write!(f, "hash mismatch: expected {expected}, got {got}")
            }
            OpDefect::Shape(error) => write!(f, "bad op shape: {error}"),
        }
    }
}

/// Validate one op file: filename, JSON, content hash, op shape, and the
/// envelope invariants (op == filename stem, ts matches filename ts). A bad
/// filename or bad JSON stops further checks.
pub(crate) fn check_op(name: &str, bytes: &[u8]) -> Vec<OpDefect> {
    let parts = match ids::parse(name) {
        Ok(p) => p,
        Err(_) => return vec![OpDefect::Filename],
    };
    let mut value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => return vec![OpDefect::Json],
    };
    match value.as_object_mut() {
        Some(obj) => {
            obj.insert("op".to_string(), Value::String(String::new()));
        }
        None => return vec![OpDefect::Json],
    }

    let mut defects = Vec::new();
    let bytes_for_hash = canonical::encode(&value);
    let recomputed = ids::hash6(&bytes_for_hash);
    if recomputed != parts.hash {
        defects.push(OpDefect::Hash {
            expected: parts.hash.clone(),
            got: recomputed,
        });
    }

    // Op-shape check: bytes parse as JSON, but do they match a known kind?
    match serde_json::from_slice::<Op>(bytes) {
        Ok(op) => {
            let op_id_from_filename = name.trim_end_matches(".json");
            if op.op_id() != op_id_from_filename {
                defects.push(OpDefect::Shape(format!(
                    "envelope op `{}` does not match filename stem `{}`",
                    op.op_id(),
                    op_id_from_filename
                )));
            }
            match op.ts().parse::<jiff::Timestamp>() {
                Ok(parsed) => {
                    let expected_basic = ids::format_op_timestamp(parsed);
                    if expected_basic != parts.ts {
                        defects.push(OpDefect::Shape(format!(
                            "envelope ts `{}` does not match filename ts `{}`",
                            op.ts(),
                            parts.ts
                        )));
                    }
                }
                Err(e) => {
                    defects.push(OpDefect::Shape(format!(
                        "envelope ts `{}` is unparseable: {e}",
                        op.ts()
                    )));
                }
            }
        }
        Err(e) => defects.push(OpDefect::Shape(format!("{e}"))),
    }
    defects
}
