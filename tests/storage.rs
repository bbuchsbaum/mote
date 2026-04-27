//! M1 integration tests: end-to-end publish round trip, EEXIST contract,
//! crash-before-link (acceptance test A3), fsck hash detection.

use std::fs;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mote::{canonical, fsck, ids, publish, repo::Store};

fn open_store_in(td: &TempDir) -> Store {
    Store::init(td.path()).unwrap()
}

#[test]
fn publish_round_trip() {
    let td = TempDir::new().unwrap();
    let store = open_store_in(&td);

    let v = json!({
        "actor": "alice",
        "kind": "create",
        "entity": "bd-01JXYZTEST",
        "set": { "title": "hello" }
    });

    let name = publish::publish_value(&store, v, jiff::Timestamp::now()).unwrap();
    let path = store.ops_dir().join(format!("{}.json", name.as_str()));
    assert!(path.is_file(), "published op file must exist in ops/");

    // tmp/ should be empty after a successful publish.
    let tmp_count = fs::read_dir(store.tmp_dir()).unwrap().count();
    assert_eq!(tmp_count, 0, "tmp/ must be empty after successful publish");

    // Re-derive the hash from the on-disk file and compare to the filename.
    let bytes = fs::read(&path).unwrap();
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("op".to_string(), Value::String(String::new()));
    let recomputed = ids::hash6(&canonical::encode(&value));
    let parts = ids::parse(name.as_str()).unwrap();
    assert_eq!(recomputed, parts.hash);
}

#[test]
fn duplicate_op_filename_is_rejected() {
    let td = TempDir::new().unwrap();
    let store = open_store_in(&td);

    let ts: jiff::Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
    let mut value = json!({"actor": "x", "kind": "create", "entity": "bd-01JTEST"});
    {
        let m = value.as_object_mut().unwrap();
        m.insert("v".into(), 1.into());
        m.insert("op".into(), "".into());
        m.insert("ts".into(), ids::format_rfc3339(ts).into());
    }
    let bytes_for_hash = canonical::encode(&value);
    let name = ids::build_op_name(ts, &bytes_for_hash);
    value
        .as_object_mut()
        .unwrap()
        .insert("op".into(), name.as_str().into());
    let bytes = canonical::encode(&value);

    publish::publish_bytes(&store, &name, &bytes).unwrap();
    let err = publish::publish_bytes(&store, &name, &bytes);
    assert!(err.is_err(), "second publish of same name must error");
    assert!(matches!(
        err.unwrap_err(),
        mote::MoteError::DuplicateOp(_) | mote::MoteError::Io(_)
    ));
}

/// Acceptance test A3: SIGKILL between fsync(tmp) and link() leaves only
/// `tmp/` junk; `ops/` is empty; `mote fsck --clean-tmp` removes the orphan.
#[test]
fn a3_crash_before_publish() {
    let td = TempDir::new().unwrap();
    let store = open_store_in(&td);

    let ts: jiff::Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
    let mut value = json!({"actor": "x", "kind": "create", "entity": "bd-01JCRASH"});
    {
        let m = value.as_object_mut().unwrap();
        m.insert("v".into(), 1.into());
        m.insert("op".into(), "".into());
        m.insert("ts".into(), ids::format_rfc3339(ts).into());
    }
    let bytes_for_hash = canonical::encode(&value);
    let name = ids::build_op_name(ts, &bytes_for_hash);
    value
        .as_object_mut()
        .unwrap()
        .insert("op".into(), name.as_str().into());
    let bytes = canonical::encode(&value);

    let tmp_path = store.tmp_dir().join(format!("{}.json", name.as_str()));
    publish::write_tmp_durable(&tmp_path, &bytes).unwrap();
    // "Crash" — we deliberately do not call link().

    // ops/ must still be empty.
    let ops_count = fs::read_dir(store.ops_dir()).unwrap().count();
    assert_eq!(ops_count, 0, "ops/ must be empty after pre-link crash");

    // tmp/ should hold our orphan.
    let tmp_count = fs::read_dir(store.tmp_dir()).unwrap().count();
    assert_eq!(tmp_count, 1);

    // Backdate the orphan to past the 1h fsck threshold and sweep.
    backdate(&tmp_path, Duration::from_secs(2 * 3600));
    let report = fsck::run(&store, true).unwrap();
    assert_eq!(report.tmp_cleaned, 1);
    let tmp_count_after = fs::read_dir(store.tmp_dir()).unwrap().count();
    assert_eq!(tmp_count_after, 0);

    // ops/ remained empty throughout.
    let ops_count_after = fs::read_dir(store.ops_dir()).unwrap().count();
    assert_eq!(ops_count_after, 0);
}

#[test]
fn fsck_detects_hash_corruption() {
    let td = TempDir::new().unwrap();
    let store = open_store_in(&td);

    let v = json!({"actor": "x", "kind": "create", "entity": "bd-01JCORRUPT"});
    let name = publish::publish_value(&store, v, jiff::Timestamp::now()).unwrap();
    let path = store.ops_dir().join(format!("{}.json", name.as_str()));

    // Tamper: flip the byte five from end (well past `{"actor":...`).
    let mut bytes = fs::read(&path).unwrap();
    let i = bytes.len() - 5;
    bytes[i] ^= 0x01;
    fs::write(&path, &bytes).unwrap();

    let report = fsck::run(&store, false).unwrap();
    assert!(!report.is_clean());
    assert_eq!(report.bad_hash.len(), 1);
}

#[test]
fn fsck_clean_skips_recent_tmp() {
    let td = TempDir::new().unwrap();
    let store = open_store_in(&td);

    // Create a fresh tmp file (simulating an in-flight publish).
    let tmp_path = store.tmp_dir().join("fresh.tmp");
    fs::write(&tmp_path, b"in-flight").unwrap();

    let report = fsck::run(&store, true).unwrap();
    // Recent tmp must NOT be cleaned (1h threshold).
    assert_eq!(report.tmp_cleaned, 0);
    assert!(tmp_path.exists());
}

fn backdate(path: &Path, by: Duration) {
    let now = std::time::SystemTime::now();
    let target = now.checked_sub(by).unwrap();
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(target).unwrap();
}
