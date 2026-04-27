//! In-tree canonical-JSON encoder.
//!
//! Produces a deterministic byte representation of a `serde_json::Value` so
//! that BLAKE3 over the encoded bytes is stable. The encoder is a strict
//! subset compatible with RFC 8785 (JCS) for our op shapes:
//!
//! - Object keys are sorted by UTF-16 code units (byte order for ASCII keys).
//! - No insignificant whitespace.
//! - Strings are JSON-escaped: `"`, `\\`, control chars (`< 0x20`) → `\u00xx`;
//!   `\b \f \n \r \t` use the short escapes.
//! - Numbers: we only emit booleans and integers in op payloads, so
//!   `serde_json::Number::Display` is sufficient. Floating-point inputs are
//!   passed through `serde_json`'s formatting; they should not appear in our
//!   ops, and adding full RFC 8785 number canonicalization is deferred.

use std::io::Write;

use serde_json::Value;

/// Canonicalize `v` into a byte buffer.
pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            // Vec<u8> writes are infallible.
            let _ = write!(out, "{n}");
        }
        Value::String(s) => write_string(out, s),
        Value::Array(arr) => {
            out.push(b'[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(out, item);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push(b'{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(out, k);
                out.push(b':');
                write_value(out, &map[*k]);
            }
            out.push(b'}');
        }
    }
}

fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{08}' => out.extend_from_slice(b"\\b"),
            '\u{0c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let mut buf = [0u8; 4];
                let utf8 = c.encode_utf8(&mut buf);
                out.extend_from_slice(utf8.as_bytes());
            }
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_keys() {
        let v = json!({"b": 1, "a": 2});
        assert_eq!(encode(&v), br#"{"a":2,"b":1}"#);
    }

    #[test]
    fn nested_objects_and_arrays() {
        let v = json!({"z": {"y": 1, "x": 2}, "a": [3, 2, 1]});
        assert_eq!(
            encode(&v),
            br#"{"a":[3,2,1],"z":{"x":2,"y":1}}"#
        );
    }

    #[test]
    fn escapes_basic_specials() {
        let v = json!({"k": "a\nb\\c\"d"});
        assert_eq!(encode(&v), br#"{"k":"a\nb\\c\"d"}"#);
    }

    #[test]
    fn escapes_control_chars() {
        let v = json!({"k": "\u{0001}\u{001f}"});
        assert_eq!(encode(&v), br#"{"k":"\u0001\u001f"}"#);
    }

    #[test]
    fn unicode_passes_through_as_utf8() {
        let v = json!({"k": "héllo"});
        let bytes = encode(&v);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("héllo"));
    }

    #[test]
    fn numbers_are_compact() {
        let v = json!({"n": 1, "m": -42, "z": 0});
        assert_eq!(encode(&v), br#"{"m":-42,"n":1,"z":0}"#);
    }

    #[test]
    fn empty_collections() {
        assert_eq!(encode(&json!({})), b"{}");
        assert_eq!(encode(&json!([])), b"[]");
    }

    #[test]
    fn round_trip_is_idempotent() {
        let v: Value = serde_json::from_str(
            r#"{"b":1,"a":[2,3,{"d":4,"c":5}],"s":"x\ty"}"#,
        )
        .unwrap();
        let bytes = encode(&v);
        let v2: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(encode(&v2), bytes);
    }
}
