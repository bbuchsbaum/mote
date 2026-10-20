//! Experimental shared authority over a single, append-only Git branch.
//!
//! The initial profile is synthetic CAS only. No task store is migrated or
//! connected by this module, and local publication semantics are unchanged.

pub mod cache;
pub mod client;
pub mod history;
pub mod planner;
pub mod protocol;
pub mod transport;
