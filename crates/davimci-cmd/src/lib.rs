//! davimci command layer: commands, undo tree, macros, project format.
//!
//! Everything that changes a timeline is an [`EditCommand`] (plan.md Phase 2,
//! Spec 10.4). One representation buys undo/redo, `.`-repeat, macros, the
//! Lua API surface, and the project file.
//!
//! Like `davimci-core`, this crate has no backend and no I/O.

pub mod command;
pub mod error;
pub mod macros;
pub mod project;
#[cfg(test)]
mod props;
pub mod session;
pub mod undo;

pub use command::{Command, EditCommand, Effect, VARIANT_NAMES};
pub use error::CmdError;
pub use macros::MacroRecorder;
pub use project::{FORMAT_VERSION, ProjectError, ProjectFile};
pub use session::Session;
pub use undo::{DEFAULT_SNAPSHOT_INTERVAL, NodeId, SavedHistory, SavedNode, UndoEntry, UndoTree};
