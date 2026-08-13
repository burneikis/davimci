//! App-level errors (every one is a finished sentence).

use davimci_cmd::CmdError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    Edit(#[from] CmdError),
    /// A frontend failed to draw. Recoverable: the app reports it and keeps
    /// editing, because a lost frame is not lost work.
    #[error("The frontend could not draw this frame: {0}.")]
    Render(String),
    /// A `:` command the app itself does not implement. The host binary
    /// (`davimci-cli`) owns the ex-command vocabulary.
    #[error("Command not handled by the editor core: {0}.")]
    UnhandledCommand(String),
    /// A command the host understood and rejected. Its message is already a
    /// finished user-facing sentence, so it is passed through unchanged -
    /// prefixing it would blame the vocabulary for a refusal.
    #[error("{0}")]
    CommandFailed(String),
}
