//! Errors at the project-lifecycle layer (plan.md Phase 8).
//!
//! Every variant carries a complete user-facing sentence, and every variant
//! is classified, because the CLI is where Phase 0's policies are actually
//! enforced: a user error refuses before touching anything, corruption stops
//! the session after flushing autosave.

use davimci_core::{Classify, ErrorClass};

/// Anything that can go wrong opening, saving, or switching projects.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("no such command: :{0}")]
    UnknownCommand(String),

    #[error(":{cmd} {usage}")]
    Usage { cmd: String, usage: String },

    #[error("this timeline has unsaved changes; use :w to save or :q! to discard")]
    UnsavedChanges,

    #[error("this timeline has no filename yet; use :w <path>")]
    NoFilename,

    #[error("there is no timeline {0} open")]
    NoSuchBuffer(String),

    #[error("no clip under the playhead to relink")]
    NothingToRelink,

    #[error("nothing in this timeline points at {0}")]
    NoClipUsesPath(String),

    #[error("there is no {what} near the playhead")]
    NoTransitionTarget { what: &'static str },

    #[error("there is no clip under the playhead to replace")]
    NothingToReplace,

    #[error("there is no clip under the playhead to {0}")]
    NoClipUnderPlayhead(&'static str),

    #[error("there is no track named {0} in this timeline")]
    NoSuchTrack(String),

    #[error("{0} needs analysis of that track; it is still running or has not started")]
    AnalysisNotReady(&'static str),

    #[error("an export to {output} is already running; wait for it or :cancel it")]
    ExportBusy { output: String },

    #[error("there is nothing to export: this timeline is empty")]
    NothingToExport,

    #[error("no export is running, so there is nothing to cancel")]
    NoExportRunning,

    #[error("the export could not start: {reason}")]
    ExportFailed { reason: String },

    /// A `BeforeExport` handler vetoed the render (spec 9.8). A user error:
    /// nothing was written and nothing was mutated.
    #[error("the export was refused: {reason}")]
    ExportRefused { reason: String },

    #[error(transparent)]
    Preset(#[from] davimci_backend::PresetError),

    #[error("could not {what} {path}: {reason}")]
    Io {
        what: &'static str,
        path: String,
        reason: String,
    },

    #[error(transparent)]
    Project(#[from] davimci_cmd::ProjectError),

    #[error(transparent)]
    Command(#[from] davimci_cmd::CmdError),

    #[error(transparent)]
    Core(#[from] davimci_core::CoreError),

    #[error(transparent)]
    Media(#[from] davimci_analysis::AnalysisError),
}

impl CliError {
    pub(crate) fn io(what: &'static str, path: impl std::fmt::Display, e: &std::io::Error) -> Self {
        Self::Io {
            what,
            path: path.to_string(),
            reason: e.to_string(),
        }
    }
}

impl Classify for CliError {
    fn class(&self) -> ErrorClass {
        match self {
            Self::UnknownCommand(_)
            | Self::Usage { .. }
            | Self::UnsavedChanges
            | Self::NoFilename
            | Self::NoSuchBuffer(_)
            | Self::NothingToRelink
            | Self::NoClipUsesPath(_)
            | Self::NoTransitionTarget { .. }
            | Self::ExportBusy { .. }
            | Self::NothingToReplace
            | Self::NoClipUnderPlayhead(_)
            | Self::NoSuchTrack(_)
            | Self::AnalysisNotReady(_)
            | Self::NothingToExport
            | Self::NoExportRunning
            | Self::ExportRefused { .. }
            | Self::Preset(_) => ErrorClass::User,
            // A failed export leaves the timeline untouched and editable.
            Self::ExportFailed { .. } => ErrorClass::Recoverable,
            // A file we cannot read or write is recoverable: the session
            // keeps running and the user can pick another path.
            Self::Io { .. } => ErrorClass::Recoverable,
            Self::Project(e) => e.class(),
            Self::Command(e) => e.class(),
            Self::Core(e) => e.class(),
            Self::Media(e) => e.class(),
        }
    }

    fn user_message(&self) -> String {
        match self {
            Self::Project(e) => e.user_message(),
            Self::Command(e) => e.user_message(),
            Self::Core(e) => e.user_message(),
            Self::Media(e) => e.user_message(),
            other => other.to_string(),
        }
    }
}
