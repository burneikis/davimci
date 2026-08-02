//! Project lifecycle: open timelines, saving, autosave, crash recovery
//! (spec §12, plan.md Phase 8).
//!
//! This is the binary's library half, and it is the first crate in the
//! workspace allowed to do I/O. Everything below it stays pure: a
//! [`workspace::Workspace`] turns files into `davimci_cmd::Session`s and back,
//! and every edit it performs still goes through a `Command`, so `:relink`
//! and an imported file are undo-tree entries like any other edit.

pub mod autosave;
pub mod error;
pub mod excmd;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use autosave::{Autosave, OnRecovery, Recovery};
pub use error::CliError;
pub use excmd::{ExCommand, ExOutcome, parse};
pub use workspace::{Buffer, Globals, Workspace};
