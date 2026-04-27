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

        let parts = match ids::parse(&name) {
            Ok(p) => p,
            Err(_) => {
                report.bad_filename.push(name);
                continue;
            }
        };

        let bytes = fs::read(entry.path())?;
        let mut value: Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => {
                report.bad_json.push(name);
                continue;
            }
        };

        let ok = match value.as_object_mut() {
            Some(obj) => {
                obj.insert("op".to_string(), Value::String(String::new()));
                true
            }
            None => false,
        };
        if !ok {
            report.bad_json.push(name);
            continue;
        }

        let bytes_for_hash = canonical::encode(&value);
        let recomputed = ids::hash6(&bytes_for_hash);
        if recomputed != parts.hash {
            report.bad_hash.push((name.clone(), parts.hash.clone(), recomputed));
        }

        // Op-shape check: bytes parse as JSON, but do they match a known kind?
        // Plus envelope invariants (op == filename stem, ts matches filename ts).
        match serde_json::from_slice::<Op>(&bytes) {
            Ok(op) => {
                let op_id_from_filename = name.trim_end_matches(".json");
                if op.op_id() != op_id_from_filename {
                    report.bad_op_shape.push((
                        name.clone(),
                        format!(
                            "envelope op `{}` does not match filename stem `{}`",
                            op.op_id(),
                            op_id_from_filename
                        ),
                    ));
                }
                match op.ts().parse::<jiff::Timestamp>() {
                    Ok(parsed) => {
                        let expected_basic = ids::format_op_timestamp(parsed);
                        if expected_basic != parts.ts {
                            report.bad_op_shape.push((
                                name.clone(),
                                format!(
                                    "envelope ts `{}` does not match filename ts `{}`",
                                    op.ts(),
                                    parts.ts
                                ),
                            ));
                        }
                    }
                    Err(e) => {
                        report.bad_op_shape.push((
                            name.clone(),
                            format!("envelope ts `{}` is unparseable: {e}", op.ts()),
                        ));
                    }
                }
            }
            Err(e) => {
                report.bad_op_shape.push((name, format!("{e}")));
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
