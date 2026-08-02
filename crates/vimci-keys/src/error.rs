//! Errors surfaced while executing a parsed action (plan.md Phase 0).

use vimci_cmd::CmdError;
use vimci_core::{Classify, ErrorClass};
use vimci_motion::MotionError;

/// Anything [`crate::engine::Engine::execute`] can fail with. Every variant
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
}

impl Classify for KeysError {
    fn class(&self) -> ErrorClass {
        match self {
            Self::Motion(e) => e.class(),
            Self::Cmd(e) => e.class(),
            Self::EmptyTarget => ErrorClass::User,
            Self::NotImplemented(_) => ErrorClass::User,
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}
