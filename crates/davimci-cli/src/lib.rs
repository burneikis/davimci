//! Project lifecycle: open timelines, saving, autosave, crash recovery
//!.
//!
//! This is the binary's library half, and it is the first crate in the
//! workspace allowed to do I/O. Everything below it stays pure: a
//! [`workspace::Workspace`] turns files into `davimci_cmd::Session`s and back,
//! and every edit it performs still goes through a `Command`, so `:relink`
//! and an imported file are undo-tree entries like any other edit.

//! It is also where the editor is *assembled*: [`editor::Editor`] is the
//! only type that holds a workspace, a render backend, a presenter and the
//! transport at once. That has to live here rather than in a frontend,
//! because no frontend may reference MLT.

pub mod analyse;
pub mod audio;
pub mod autosave;
pub mod editor;
pub mod error;
pub mod excmd;
pub mod export;
pub mod plugins;
pub mod setting;
pub mod thumbnail;
pub mod transport;
#[cfg(feature = "tui")]
pub mod tui;
#[cfg(feature = "window")]
pub mod window;
pub mod workspace;

#[cfg(test)]
mod tests;

pub use analyse::Analyser;
pub use audio::{FadeEnd, duck_plan, loud_spans};
pub use autosave::{Autosave, OnRecovery, Recovery};
pub use editor::Editor;
pub use error::CliError;
pub use excmd::{ExCommand, ExOutcome, parse, vocabulary};
pub use export::{ExportEvent, Exporter};
pub use plugins::{AskOnTerminal, Plugins};
pub use setting::Setting;
pub use transport::{Transport, TransportState};
#[cfg(feature = "window")]
pub use window::Window;
pub use workspace::{Buffer, Globals, Workspace};
