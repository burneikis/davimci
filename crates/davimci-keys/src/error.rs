//! Errors surfaced while executing a parsed action.

use davimci_cmd::CmdError;
use davimci_core::{Classify, ErrorClass};
use davimci_motion::MotionError;

/// Anything `Engine::execute` can fail with. Every variant
/// is a user-facing sentence; nothing here is `Debug` output.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum KeysError {
    #[error(transparent)]
    Motion(#[from] MotionError),

    #[error(transparent)]
    Cmd(#[from] CmdError),

    #[error("there is nothing to act on")]
    EmptyTarget,

    #[error("that is not available yet: {0}")]
    NotImplemented(&'static str),

    /// A dispatch arm that should be impossible was reached. A bug, but not
    /// a corrupt timeline: nothing was mutated, so the editor degrades
    /// locally and keeps going instead of
    /// panicking in a library crate.
    #[error("davimci could not carry that out: {0}")]
    Internal(&'static str),
}

impl Classify for KeysError {
    fn class(&self) -> ErrorClass {
        match self {
            Self::Motion(e) => e.class(),
            Self::Cmd(e) => e.class(),
            Self::EmptyTarget | Self::NotImplemented(_) => ErrorClass::User,
            Self::Internal(_) => ErrorClass::Recoverable,
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}
