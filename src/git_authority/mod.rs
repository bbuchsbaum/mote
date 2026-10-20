//! Experimental shared authority over a single, append-only Git branch.
//!
//! Development profiles cover synthetic CAS, coordination core and lifecycle.
//! Empty stores can explicitly bootstrap or connect; existing local histories
//! require a separately qualified migration. Connected stores fence local writes.

pub mod backup;
pub mod cache;
pub mod cli;
pub mod client;
pub mod connection;
pub mod coordination;
pub mod history;
pub mod legacy;
pub mod legacy_profile;
pub mod planner;
pub mod protocol;
pub mod transport;
