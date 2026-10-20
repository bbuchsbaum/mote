//! Experimental shared authority over a single, append-only Git branch.
//!
//! Development profiles cover synthetic CAS, coordination core and lifecycle.
//! Empty stores can explicitly bootstrap or connect; existing local histories
//! require a separately qualified migration. Connected stores fence local writes.

pub mod cache;
pub mod client;
pub mod connection;
pub mod coordination;
pub mod history;
pub mod planner;
pub mod protocol;
pub mod transport;
