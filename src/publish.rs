//! Maildir-style op publication.
//!
//! Steps for every op:
//!   1. Build canonical-JSON bytes.
//!   2. Write to `.mote/tmp/<name>.json` with `O_CREAT|O_EXCL`.
//!   3. fsync the temp file.
//!   4. `link()` from `tmp/<name>.json` to `ops/<name>.json` (fails on EEXIST,
//!      which is the contract: never silently overwrite a published op).
//!   5. fsync the `ops/` directory so the entry is durable.
//!   6. unlink the tmp file (debris if this fails is harmless).
//!
//! `publish_value` is the typical entry point; `write_tmp_durable` and
//! `publish_bytes` are exposed for tests and for the in-tree callers that
//! already hold canonical bytes (e.g. the future M2 op-typed publishers).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use jiff::Timestamp;
use serde::Serialize;
use serde_json::Value;

use crate::canonical;
use crate::errors::{MoteError, MoteResult};
use crate::ids::{self, OpName};
use crate::repo::Store;

/// Publish a value-shaped op. The provided value MUST be a JSON object; this
/// function fills in `v=1`, `ts`, and `op` (the latter computed from canonical
/// bytes), then publishes via the Maildir protocol.
///
/// Returns the assigned op name on success.
pub fn publish_value(
    store: &Store,
    mut op_value: Value,
    ts: Timestamp,
) -> MoteResult<OpName> {
    {
        let obj = op_value
            .as_object_mut()
            .ok_or_else(|| MoteError::Other("publish_value: expected JSON object".into()))?;
        obj.insert("v".to_string(), Value::Number(1.into()));
        obj.insert("op".to_string(), Value::String(String::new()));
        obj.insert("ts".to_string(), Value::String(ids::format_rfc3339(ts)));
    }

    let bytes_for_hash = canonical::encode(&op_value);
    let name = ids::build_op_name(ts, &bytes_for_hash);

    op_value
        .as_object_mut()
        .expect("checked above")
        .insert("op".to_string(), Value::String(name.as_str().to_string()));

    let bytes_final = canonical::encode(&op_value);
    publish_bytes(store, &name, &bytes_final)?;
    Ok(name)
}

/// Publish a typed op. The op MUST have its envelope filled in (`v`, `op=""`,
/// `ts`, `actor`, plus kind-specific fields). The `op` field is overwritten
/// with the assigned op-name and the op is published.
///
/// `ts` is taken from the op's `ts` string (parsed back to a Timestamp), so
/// callers should set it once when constructing the op.
pub fn publish_op<T: Serialize>(store: &Store, op: &T) -> MoteResult<OpName> {
    let mut value = serde_json::to_value(op)?;

    // Validate shape and zero out the op field for hashing.
    let ts_str = match value.as_object() {
        Some(obj) => obj
            .get("ts")
            .and_then(|v| v.as_str())
            .ok_or_else(|| MoteError::Other("publish_op: missing `ts` string".into()))?
            .to_string(),
        None => return Err(MoteError::Other("publish_op: expected JSON object".into())),
    };
    let ts: Timestamp = ts_str
        .parse()
        .map_err(|e| MoteError::Other(format!("publish_op: bad ts {ts_str}: {e}")))?;

    value
        .as_object_mut()
        .expect("checked above")
        .insert("op".to_string(), Value::String(String::new()));

    let bytes_for_hash = canonical::encode(&value);
    let name = ids::build_op_name(ts, &bytes_for_hash);

    value
        .as_object_mut()
        .expect("checked above")
        .insert("op".to_string(), Value::String(name.as_str().to_string()));

    let bytes_final = canonical::encode(&value);
    publish_bytes(store, &name, &bytes_final)?;
    Ok(name)
}

/// Publish raw canonical bytes as `<name>.json` in the store. Bytes are NOT
/// validated; this is the low-level Maildir-style publish.
pub fn publish_bytes(store: &Store, name: &OpName, bytes: &[u8]) -> MoteResult<()> {
    let filename = format!("{}.json", name.as_str());
    let tmp_path = store.tmp_dir().join(&filename);
    let ops_path = store.ops_dir().join(&filename);

    write_tmp_durable(&tmp_path, bytes)?;

    // Fail loudly on EEXIST: the contract is that op names are unique.
    std::fs::hard_link(&tmp_path, &ops_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            MoteError::DuplicateOp(name.as_str().to_string())
        } else {
            MoteError::Io(e)
        }
    })?;

    fsync_dir(&store.ops_dir())?;
    let _ = std::fs::remove_file(&tmp_path); // tmp/ debris is harmless

    Ok(())
}

/// Write `bytes` to `tmp_path` with `O_CREAT|O_EXCL|O_WRONLY`, fsync, and close.
/// Exposed so the crash-injection test can simulate "crash after fsync, before link".
pub fn write_tmp_durable(tmp_path: &Path, bytes: &[u8]) -> MoteResult<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL on Unix
        .open(tmp_path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    Ok(())
}

/// Open a directory and fsync it so that the entry rename/link is durable.
/// On Linux/macOS, opening a dir read-only and calling `fsync()` flushes its
/// metadata. (For stronger durability on APFS, `F_FULLFSYNC` is preferred,
/// but plain fsync is sufficient for our crash-correctness contract.)
pub fn fsync_dir(dir: &Path) -> MoteResult<()> {
    let f = File::open(dir)?;
    f.sync_all()?;
    Ok(())
}
