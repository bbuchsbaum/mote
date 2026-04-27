//! Crate-level error type. We deliberately use a small handwritten enum (no
//! `thiserror` dependency); the variants map cleanly to the spec's exit codes.

use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum MoteError {
    Io(std::io::Error),
    Json(serde_json::Error),
    StoreNotFound(PathBuf),
    StoreAlreadyInitialized(PathBuf),
    InvalidOpName(String),
    HashMismatch { file: String, expected: String, got: String },
    DuplicateOp(String),
    ActorUnresolved,
    Rejected(String),
    /// User-facing validation error (bad flag value, missing required field,
    /// unknown subject, etc.). Maps to CLI exit code 3.
    Invalid(String),
    /// Unknown / internal failure. Maps to CLI exit code 1.
    Other(String),
}

pub type MoteResult<T> = Result<T, MoteError>;

impl fmt::Display for MoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoteError::Io(e) => write!(f, "io error: {e}"),
            MoteError::Json(e) => write!(f, "json error: {e}"),
            MoteError::StoreNotFound(p) => write!(f, "store not found: walked up from {}", p.display()),
            MoteError::StoreAlreadyInitialized(p) => {
                write!(f, "store already initialized at {}", p.display())
            }
            MoteError::InvalidOpName(s) => write!(f, "invalid op filename: {s}"),
            MoteError::HashMismatch { file, expected, got } => {
                write!(f, "hash mismatch in {file}: expected {expected}, got {got}")
            }
            MoteError::DuplicateOp(s) => write!(f, "duplicate op id: {s}"),
            MoteError::ActorUnresolved => write!(f, "actor identity unresolved"),
            MoteError::Rejected(s) => write!(f, "op rejected: {s}"),
            MoteError::Invalid(s) => write!(f, "{s}"),
            MoteError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for MoteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MoteError::Io(e) => Some(e),
            MoteError::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MoteError {
    fn from(e: std::io::Error) -> Self {
        MoteError::Io(e)
    }
}

impl From<serde_json::Error> for MoteError {
    fn from(e: serde_json::Error) -> Self {
        MoteError::Json(e)
    }
}
