//! mote — immutable op-maildir issue + coordination tracker.
//!
//! M1 surfaces: storage primitives only. Higher planes (issue / path / message)
//! are layered in M2+.

pub mod canonical;
pub mod cli;
pub mod errors;
pub mod events;
pub mod fsck;
pub mod ids;
pub mod op;
pub mod paths;
pub mod publish;
pub mod reducer;
pub mod repo;
pub mod state;
pub mod tui;
pub mod watch;

pub use errors::{MoteError, MoteResult};
pub use repo::Store;
