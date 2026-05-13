//! ID helpers: bead/reservation/message ULID-prefixed ids, plus the op-name
//! format used as the filename in `.mote/ops/`.
//!
//! Op-name shape:
//!   `<UTC ISO basic, microsec>-p<pid>-c<counter4hex>-r<rand4hex>-h<hash6hex>`
//!
//! Example:
//!   `20260420T182455.124583Z-p4312-c0007-r7f3a-h6b9d0`

use std::sync::atomic::{AtomicU32, Ordering};

use jiff::Timestamp;
use rand::RngCore;
use ulid::Ulid;

use crate::errors::{MoteError, MoteResult};

static COUNTER: AtomicU32 = AtomicU32::new(0);

pub fn new_bead_id() -> String {
    format!("bd-{}", Ulid::new())
}

/// Validate a user-supplied bead id (e.g. when migrating from another tracker
/// via `mote new --id`). Mote-minted ids start with `bd-`; external ids must
/// not, so the two namespaces stay distinguishable in op files and CLI output.
///
/// Rules:
/// - 1..=64 chars
/// - lowercase ascii alphanumerics, `-`, or `_`
/// - first and last char must be alphanumeric
/// - must NOT start with `bd-` (reserved for mote-minted ids)
pub fn validate_external_bead_id(id: &str) -> MoteResult<()> {
    if id.is_empty() {
        return Err(MoteError::Invalid("--id must be non-empty".into()));
    }
    if id.len() > 64 {
        return Err(MoteError::Invalid(format!("--id `{id}` exceeds 64 chars")));
    }
    if id.starts_with("bd-") {
        return Err(MoteError::Invalid(
            "--id `bd-...` prefix is reserved for mote-minted ids".into(),
        ));
    }
    let bytes = id.as_bytes();
    let is_alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let is_body = |b: u8| is_alnum(b) || b == b'-' || b == b'_';
    if !is_alnum(bytes[0]) || !is_alnum(bytes[bytes.len() - 1]) {
        return Err(MoteError::Invalid(format!(
            "--id `{id}` must start and end with [a-z0-9]"
        )));
    }
    if !bytes.iter().all(|&b| is_body(b)) {
        return Err(MoteError::Invalid(format!(
            "--id `{id}` may only contain [a-z0-9_-]"
        )));
    }
    Ok(())
}

pub fn new_reservation_id() -> String {
    format!("rv-{}", Ulid::new())
}

pub fn new_msg_id() -> String {
    format!("msg-{}", Ulid::new())
}

pub fn new_post_id() -> String {
    format!("post-{}", Ulid::new())
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OpName(String);

impl OpName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// Wrap an existing string after validating it parses.
    pub fn from_string(s: String) -> MoteResult<Self> {
        parse(&s)?;
        Ok(OpName(s))
    }
}

#[derive(Debug, Clone)]
pub struct OpNameParts {
    pub ts: String,
    pub pid: String,
    pub counter: String,
    pub rand: String,
    pub hash: String,
}

/// Format a `Timestamp` as the basic-form UTC microsecond string used in op names:
/// `YYYYMMDDTHHMMSS.UUUUUUZ` (23 chars).
pub fn format_op_timestamp(ts: Timestamp) -> String {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::UTC);
    let dt = zoned.datetime();
    let micros: i32 = dt.subsec_nanosecond() / 1_000;
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}.{:06}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        micros,
    )
}

/// Format a `Timestamp` as RFC3339 microsecond UTC: `YYYY-MM-DDTHH:MM:SS.UUUUUUZ`.
pub fn format_rfc3339(ts: Timestamp) -> String {
    let zoned = ts.to_zoned(jiff::tz::TimeZone::UTC);
    let dt = zoned.datetime();
    let micros: i32 = dt.subsec_nanosecond() / 1_000;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        micros,
    )
}

fn next_counter() -> u16 {
    let v = COUNTER.fetch_add(1, Ordering::Relaxed);
    (v & 0xFFFF) as u16
}

fn rand4() -> u16 {
    let mut buf = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut buf);
    u16::from_be_bytes(buf)
}

/// First 6 hex chars (3 BLAKE3 bytes) of `BLAKE3(bytes)`.
pub fn hash6(bytes: &[u8]) -> String {
    let h = blake3::hash(bytes);
    let b = h.as_bytes();
    format!("{:02x}{:02x}{:02x}", b[0], b[1], b[2])
}

/// Build an op name from a timestamp and the canonical-JSON bytes of the op
/// with its `op` field set to the empty string.
pub fn build_op_name(ts: Timestamp, bytes_for_hash: &[u8]) -> OpName {
    let ts_str = format_op_timestamp(ts);
    let pid = format!("{:04}", std::process::id());
    let counter = format!("{:04x}", next_counter());
    let rand = format!("{:04x}", rand4());
    let hash = hash6(bytes_for_hash);
    OpName(format!("{ts_str}-p{pid}-c{counter}-r{rand}-h{hash}"))
}

/// Parse an op filename (with or without `.json` suffix) into its parts.
pub fn parse(name: &str) -> MoteResult<OpNameParts> {
    let stem = name.strip_suffix(".json").unwrap_or(name);
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() != 5 {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    let ts = parts[0];
    let p = parts[1];
    let c = parts[2];
    let r = parts[3];
    let h = parts[4];

    // Timestamp: YYYYMMDDTHHMMSS.UUUUUUZ → 23 chars.
    if ts.len() != 23 || !ts.ends_with('Z') {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    if !p.starts_with('p') || p.len() < 5 {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    if !c.starts_with('c') || c.len() != 5 {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    if !r.starts_with('r') || r.len() != 5 {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    if !h.starts_with('h') || h.len() != 7 {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    if !is_hex(&c[1..]) || !is_hex(&r[1..]) || !is_hex(&h[1..]) {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }
    if !p[1..].chars().all(|ch| ch.is_ascii_digit()) {
        return Err(MoteError::InvalidOpName(name.to_string()));
    }

    Ok(OpNameParts {
        ts: ts.to_string(),
        pid: p[1..].to_string(),
        counter: c[1..].to_string(),
        rand: r[1..].to_string(),
        hash: h[1..].to_string(),
    })
}

fn is_hex(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_formats_match_spec() {
        let ts: Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
        assert_eq!(format_op_timestamp(ts), "20260420T182455.124583Z");
        assert_eq!(format_rfc3339(ts), "2026-04-20T18:24:55.124583Z");
    }

    #[test]
    fn timestamp_zero_subsec() {
        let ts: Timestamp = "2026-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(format_op_timestamp(ts), "20260101T000000.000000Z");
    }

    #[test]
    fn op_name_round_trip() {
        let ts: Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
        let name = build_op_name(ts, b"\"some-bytes\"");
        let parts = parse(name.as_str()).unwrap();
        assert_eq!(parts.ts, "20260420T182455.124583Z");
        assert_eq!(parts.counter.len(), 4);
        assert_eq!(parts.rand.len(), 4);
        assert_eq!(parts.hash.len(), 6);
    }

    #[test]
    fn parse_accepts_json_suffix() {
        let ts: Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
        let name = build_op_name(ts, b"x");
        let with_suffix = format!("{}.json", name.as_str());
        assert!(parse(&with_suffix).is_ok());
    }

    #[test]
    fn parse_rejects_short_name() {
        assert!(parse("not-an-op-name").is_err());
    }

    #[test]
    fn parse_rejects_bad_hex_in_counter() {
        let bad = "20260420T182455.124583Z-p4312-cZZZZ-r7f3a-h6b9d0";
        assert!(parse(bad).is_err());
    }

    #[test]
    fn parse_rejects_short_timestamp() {
        let bad = "20260420T182455Z-p4312-c0007-r7f3a-h6b9d0";
        assert!(parse(bad).is_err());
    }

    #[test]
    fn parse_rejects_non_digit_pid() {
        let bad = "20260420T182455.124583Z-pXXXX-c0007-r7f3a-h6b9d0";
        assert!(parse(bad).is_err());
    }

    #[test]
    fn names_sort_lex_in_time_order() {
        let t1: Timestamp = "2026-01-01T00:00:00.000000Z".parse().unwrap();
        let t2: Timestamp = "2026-12-31T23:59:59.999999Z".parse().unwrap();
        let n1 = build_op_name(t1, b"a");
        let n2 = build_op_name(t2, b"b");
        assert!(
            n1.as_str() < n2.as_str(),
            "{} should sort before {}",
            n1.as_str(),
            n2.as_str()
        );
    }

    #[test]
    fn id_prefixes() {
        assert!(new_bead_id().starts_with("bd-"));
        assert!(new_reservation_id().starts_with("rv-"));
        assert!(new_msg_id().starts_with("msg-"));
        assert!(new_post_id().starts_with("post-"));
    }

    #[test]
    fn validate_external_bead_id_accepts_typical_external_ids() {
        for id in [
            "psycloud-eqgu",
            "abc",
            "a",
            "a1",
            "issue_42",
            "x-y-z",
            "1abc",
            "with_under_score",
        ] {
            assert!(
                validate_external_bead_id(id).is_ok(),
                "expected {id} to validate"
            );
        }
    }

    #[test]
    fn validate_external_bead_id_rejects_bad_inputs() {
        for id in [
            "",                  // empty
            "bd-anything",       // reserved prefix
            "-leading-hyphen",   // bad start
            "trailing-hyphen-",  // bad end
            "_underscore_start", // bad start
            "Has-Capitals",      // uppercase
            "spaces in id",      // space
            "slash/in/id",       // slash
            "dot.in.id",         // dot
            "exclaim!",          // punctuation
        ] {
            assert!(
                validate_external_bead_id(id).is_err(),
                "expected {id:?} to fail validation"
            );
        }
    }

    #[test]
    fn validate_external_bead_id_rejects_too_long() {
        let id = "a".repeat(65);
        assert!(validate_external_bead_id(&id).is_err());
        let ok = "a".repeat(64);
        assert!(validate_external_bead_id(&ok).is_ok());
    }

    #[test]
    fn hash6_is_six_hex_chars() {
        let h = hash6(b"hello");
        assert_eq!(h.len(), 6);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn counter_is_unique_per_invocation() {
        let ts: Timestamp = "2026-04-20T18:24:55.124583Z".parse().unwrap();
        let a = build_op_name(ts, b"x");
        let b = build_op_name(ts, b"x");
        // Same ts + same bytes → same hash, but counter differs → distinct names.
        assert_ne!(a.as_str(), b.as_str());
    }
}
