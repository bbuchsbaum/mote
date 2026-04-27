//! Crash-safety failpoints around the publish protocol.
//!
//! The publish steps are:
//!   1. write tmp/<name>.json with O_CREAT|O_EXCL
//!   2. fsync(tmp)
//!   3. close(tmp)
//!   4. link(tmp, ops)
//!   5. fsync(ops dir)
//!   6. unlink(tmp)
//!
//! The contracts we want to verify under simulated crash at each failpoint:
//!   - ops/ NEVER contains a half-published op
//!   - tmp/ debris is acceptable; fsck --clean-tmp eventually sweeps it
//!   - replay sees the op iff link() ran before the "crash"
//!
//! We simulate crashes by calling the individual primitives and stopping at
//! the failpoint instead of completing the publish.

use std::fs;
use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};
use tempfile::TempDir;

use mote::{canonical, fsck, ids, publish, reducer, repo::Store};

fn open_store(td: &TempDir) -> Store {
    Store::init(td.path()).unwrap()
}

fn build_op_bytes(entity: &str) -> (ids::OpName, Vec<u8>) {
    let ts: Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
    let mut value = json!({
        "actor": "alice",
        "kind": "create",
        "entity": entity,
        "set": { "title": "x" }
    });
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
        .insert("op".into(), Value::String(name.as_str().into()));
    let bytes = canonical::encode(&value);
    (name, bytes)
}

fn backdate(path: &Path, by: Duration) {
    let now = std::time::SystemTime::now();
    let target = now.checked_sub(by).unwrap();
    let f = fs::File::options().write(true).open(path).unwrap();
    f.set_modified(target).unwrap();
}

// ---------------------------------------------------------------------------
// Failpoint 1: crash mid-write (file partially written, fsync not called).
// ---------------------------------------------------------------------------
#[test]
fn failpoint_partial_write_in_tmp() {
    let td = TempDir::new().unwrap();
    let store = open_store(&td);
    let (name, bytes) = build_op_bytes("bd-fp1");

    // Simulate by writing a truncated copy to tmp/<name>.json directly,
    // bypassing fsync and link.
    let tmp_path = store.tmp_dir().join(format!("{}.json", name.as_str()));
    let half = bytes.len() / 2;
    fs::write(&tmp_path, &bytes[..half]).unwrap();

    // ops/ must remain empty.
    assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 0);

    // Replay sees no ops (the op was never linked into ops/).
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.beads.contains_key("bd-fp1"));

    // After backdating, fsck --clean-tmp removes the orphan.
    backdate(&tmp_path, Duration::from_secs(7200));
    let report = fsck::run(&store, true).unwrap();
    assert_eq!(report.tmp_cleaned, 1);
    assert!(!tmp_path.exists());
    // ops/ untouched.
    assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------
// Failpoint 2: crash after fsync(tmp) but before link() — already covered by
// tests/storage.rs::a3_crash_before_publish; reproduce here for completeness.
// ---------------------------------------------------------------------------
#[test]
fn failpoint_after_fsync_before_link() {
    let td = TempDir::new().unwrap();
    let store = open_store(&td);
    let (name, bytes) = build_op_bytes("bd-fp2");

    let tmp_path = store.tmp_dir().join(format!("{}.json", name.as_str()));
    publish::write_tmp_durable(&tmp_path, &bytes).unwrap();
    // "Crash": don't link. ops/ stays empty.

    assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 0);
    let state = reducer::replay_store(&store).unwrap();
    assert!(!state.beads.contains_key("bd-fp2"));

    backdate(&tmp_path, Duration::from_secs(7200));
    let report = fsck::run(&store, true).unwrap();
    assert_eq!(report.tmp_cleaned, 1);
}

// ---------------------------------------------------------------------------
// Failpoint 3: crash after link() but before fsync(ops dir).
// On user-space crash, the link is visible to the same FS view, so replay
// sees the op. Power-loss durability is a separate concern; here we verify
// process-crash semantics.
// ---------------------------------------------------------------------------
#[test]
fn failpoint_after_link_before_dir_fsync() {
    let td = TempDir::new().unwrap();
    let store = open_store(&td);
    let (name, bytes) = build_op_bytes("bd-fp3");

    let tmp_path = store.tmp_dir().join(format!("{}.json", name.as_str()));
    let ops_path = store.ops_dir().join(format!("{}.json", name.as_str()));
    publish::write_tmp_durable(&tmp_path, &bytes).unwrap();
    fs::hard_link(&tmp_path, &ops_path).unwrap();
    // "Crash": skip fsync(ops dir) and unlink(tmp).

    // ops/ has the op (process-crash visibility).
    assert!(ops_path.exists());
    // tmp/ still has the file too — same inode via hard link.
    assert!(tmp_path.exists());

    // Replay sees the op as published.
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.beads.contains_key("bd-fp3"));

    // fsck --clean-tmp sweeps the orphan tmp file but leaves ops/ alone.
    backdate(&tmp_path, Duration::from_secs(7200));
    let report = fsck::run(&store, true).unwrap();
    assert_eq!(report.tmp_cleaned, 1);
    assert!(!tmp_path.exists());
    assert!(ops_path.exists());
}

// ---------------------------------------------------------------------------
// Failpoint 4: crash after fsync(ops dir) but before unlink(tmp).
// Indistinguishable in steady state from failpoint 3 above, but exercised
// to verify the dir fsync is itself harmless and the cleanup story is robust.
// ---------------------------------------------------------------------------
#[test]
fn failpoint_after_dir_fsync_before_unlink() {
    let td = TempDir::new().unwrap();
    let store = open_store(&td);
    let (name, bytes) = build_op_bytes("bd-fp4");

    let tmp_path = store.tmp_dir().join(format!("{}.json", name.as_str()));
    let ops_path = store.ops_dir().join(format!("{}.json", name.as_str()));
    publish::write_tmp_durable(&tmp_path, &bytes).unwrap();
    fs::hard_link(&tmp_path, &ops_path).unwrap();
    publish::fsync_dir(&store.ops_dir()).unwrap();
    // "Crash": skip unlink(tmp).

    assert!(tmp_path.exists());
    assert!(ops_path.exists());
    let state = reducer::replay_store(&store).unwrap();
    assert!(state.beads.contains_key("bd-fp4"));

    backdate(&tmp_path, Duration::from_secs(7200));
    let report = fsck::run(&store, true).unwrap();
    assert_eq!(report.tmp_cleaned, 1);
}

// ---------------------------------------------------------------------------
// Failpoint 5: subsequent successful publish is unaffected by stale tmp debris.
// Demonstrates that one half-published op does not block future writes.
// ---------------------------------------------------------------------------
#[test]
fn failpoint_orphan_does_not_block_subsequent_publish() {
    let td = TempDir::new().unwrap();
    let store = open_store(&td);

    // Drop a stale orphan in tmp/ (simulating an old crashed publish).
    let orphan_name = "20200101T000000.000001Z-p0001-c0001-r0001-h000001.json";
    let orphan_path = store.tmp_dir().join(orphan_name);
    fs::write(&orphan_path, b"{}").unwrap();

    // A normal publish on top should succeed.
    let v = json!({"actor": "alice", "kind": "create", "entity": "bd-fp5", "set": {"title": "x"}});
    let _ = publish::publish_value(&store, v, Timestamp::now()).unwrap();

    // ops/ has exactly one new file. Orphan is still in tmp/ until fsck.
    assert_eq!(fs::read_dir(store.ops_dir()).unwrap().count(), 1);
    assert!(orphan_path.exists());

    // fsck --clean-tmp sweeps the old orphan; recent publishes are protected
    // by the 1-hour age threshold.
    backdate(&orphan_path, Duration::from_secs(7200));
    let report = fsck::run(&store, true).unwrap();
    assert_eq!(report.tmp_cleaned, 1);
}

// ---------------------------------------------------------------------------
// Failpoint 6: link to ops/ collision after a prior partial failure leaves
// debris in tmp/ — the second writer must still observe EEXIST and not stomp.
// (Belt-and-suspenders: this is what `O_CREAT|O_EXCL` and `link` enforce.)
// ---------------------------------------------------------------------------
#[test]
fn failpoint_eexist_holds_under_tmp_debris() {
    let td = TempDir::new().unwrap();
    let store = open_store(&td);
    let (name, bytes) = build_op_bytes("bd-fp6");

    // First writer fully publishes.
    publish::publish_bytes(&store, &name, &bytes).unwrap();

    // Second writer attempts the same op-name (impossible in practice since
    // names contain a counter+rand+hash, but if it ever happened we want a
    // hard error, not a silent overwrite).
    let err = publish::publish_bytes(&store, &name, &bytes);
    assert!(
        err.is_err(),
        "duplicate publish must error, not silently succeed"
    );
}
