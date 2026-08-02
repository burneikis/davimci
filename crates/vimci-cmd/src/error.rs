//! Errors raised by the command layer (plan.md Phase 0 classes).

use vimci_core::{Classify, CoreError, ErrorClass};

/// Anything that can go wrong executing, undoing, or replaying a command.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CmdError {
    /// The timeline rejected the edit. Nothing was mutated.
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error("already at the oldest change")]
    NothingToUndo,

    #[error("already at the newest change")]
    NothingToRedo,

    #[error("there is no edit to repeat")]
    NothingToRepeat,

    #[error("register {0} holds no macro")]
    NoSuchMacro(char),

    #[error("no macro is being recorded")]
    NotRecording,

    #[error("a macro is already being recorded into register {0}")]
    AlreadyRecording(char),

    /// A command in the log could not be replayed. The undo history is no
    /// longer trustworthy, so the last snapshot has to take over.
    #[error("the edit history is inconsistent and could not be replayed: {0}")]
    ReplayFailed(String),
}

impl Classify for CmdError {
    fn class(&self) -> ErrorClass {
        match self {
            Self::Core(e) => e.class(),
            Self::NothingToUndo
            | Self::NothingToRedo
            | Self::NothingToRepeat
            | Self::NoSuchMacro(_)
            | Self::NotRecording
            | Self::AlreadyRecording(_) => ErrorClass::User,
            Self::ReplayFailed(_) => ErrorClass::Corruption,
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_mistakes_are_user_errors() {
        assert_eq!(CmdError::NothingToUndo.class(), ErrorClass::User);
        assert_eq!(CmdError::NoSuchMacro('a').class(), ErrorClass::User);
    }

    #[test]
    fn a_core_rejection_keeps_its_class() {
        let e = CmdError::from(CoreError::ZeroDuration);
        assert_eq!(e.class(), ErrorClass::User);
        assert!(!e.user_message().is_empty());
    }

    #[test]
    fn replay_failure_is_corruption() {
        let e = CmdError::ReplayFailed("bad inverse".into());
        assert_eq!(e.class(), ErrorClass::Corruption);
        assert!(!e.class().is_continuable());
    }
}
