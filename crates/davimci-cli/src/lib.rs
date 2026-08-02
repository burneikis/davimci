//! Project lifecycle: open timelines, saving, autosave, crash recovery
//! (spec §12, plan.md Phase 8).
//!
//! This is the binary's library half, and it is the first crate in the
//! workspace allowed to do I/O. Everything below it stays pure: a
//! [`workspace::Workspace`] turns files into `davimci_cmd::Session`s and back,
//! and every edit it performs still goes through a `Command`, so `:relink`
//! and an imported file are undo-tree entries like any other edit.

//! It is also where the editor is *assembled*: [`editor::Editor`] is the
//! only type that holds a workspace, a render backend, a presenter and the
//! transport at once. That has to live here rather than in a frontend,
//! because no frontend may reference MLT (spec §10.1).

pub mod autosave;
pub mod editor;
pub mod error;
pub mod excmd;
pub mod transport;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use autosave::{Autosave, OnRecovery, Recovery};
pub use editor::Editor;
pub use error::CliError;
pub use excmd::{ExCommand, ExOutcome, parse};
pub use transport::{Transport, TransportState};
pub use workspace::{Buffer, Globals, Workspace};
