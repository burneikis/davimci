//! Error model for vimci (plan.md Phase 0).
//!
//! Four error classes, each with a fixed recovery policy. Every error must be
//! classifiable, and every error must carry text fit for the status line -
//! raw `Debug` output must never reach the user.

use std::fmt;

/// How the application must respond to an error.
///
/// This is the contract from plan.md Phase 0. Frontends switch on this to
/// decide between "show a message", "keep going degraded", and "save and die".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The user asked for something impossible. Reject before mutating; the
    /// timeline is untouched and nothing enters the undo log.
    User,
    /// Source media is missing or unreadable. The project stays open and
    /// editable; affected clips are flagged offline and export is blocked.
    OfflineMedia,
    /// A local, contained failure. Degrade (black frame, failed analysis,
    /// disabled Lua handler), notify, and keep editing.
    Recoverable,
    /// An invariant broke or state is corrupt. Flush autosave and exit; the
    /// periodic snapshot bounds the loss.
    Corruption,
}

impl ErrorClass {
    /// Whether the editor may keep running after an error of this class.
    #[must_use]
    pub fn is_continuable(self) -> bool {
        !matches!(self, Self::Corruption)
    }
}

/// Any vimci error can report its class and a user-facing message.
pub trait Classify {
    fn class(&self) -> ErrorClass;
    /// Text shown in the status line. Must be a complete, human sentence.
    fn user_message(&self) -> String;
}

/// Errors originating in the timeline model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("no clip at frame {frame} on track {track}")]
    NoClipAtPlayhead { track: String, frame: u64 },

    #[error("cannot trim past the end of the source (short by {shortfall} frames)")]
    InsufficientHandles { shortfall: u64 },

    #[error("track {0} does not exist")]
    NoSuchTrack(String),

    #[error("timeline has no framerate set")]
    NoFramerate,

    #[error("{source_fps} fps does not conform to the {timeline_fps} fps timeline")]
    Unconformable {
        source_fps: String,
        timeline_fps: String,
    },

    #[error("source media is offline: {path}")]
    OfflineMedia { path: String },

    #[error("timeline invariant violated: {0}")]
    InvariantViolation(String),
}

impl Classify for CoreError {
    fn class(&self) -> ErrorClass {
        match self {
            Self::NoClipAtPlayhead { .. }
            | Self::InsufficientHandles { .. }
            | Self::NoSuchTrack(_)
            | Self::NoFramerate
            | Self::Unconformable { .. } => ErrorClass::User,
            Self::OfflineMedia { .. } => ErrorClass::OfflineMedia,
            Self::InvariantViolation(_) => ErrorClass::Corruption,
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

/// Assert a timeline invariant.
///
/// The single sanctioned panic path in library code, per plan.md Phase 0 rule
/// 2. Prefer returning [`CoreError::InvariantViolation`] where a caller can
/// meaningfully handle it; use this only for conditions that indicate a bug.
#[macro_export]
macro_rules! assert_invariant {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            panic!("vimci invariant violated: {}", format_args!($($arg)+));
        }
    };
}

/// A non-fatal notice destined for the status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub class: ErrorClass,
    pub text: String,
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.text)
    }
}

impl Notice {
    pub fn from_error<E: Classify>(err: &E) -> Self {
        Self {
            class: err.class(),
            text: err.user_message(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_errors_are_continuable() {
        let e = CoreError::NoSuchTrack("A9".into());
        assert_eq!(e.class(), ErrorClass::User);
        assert!(e.class().is_continuable());
    }

    #[test]
    fn corruption_is_not_continuable() {
        let e = CoreError::InvariantViolation("overlapping clips".into());
        assert_eq!(e.class(), ErrorClass::Corruption);
        assert!(!e.class().is_continuable());
    }

    #[test]
    fn offline_media_is_its_own_class() {
        let e = CoreError::OfflineMedia {
            path: "/tmp/gone.mkv".into(),
        };
        assert_eq!(e.class(), ErrorClass::OfflineMedia);
        assert!(e.class().is_continuable());
    }

    /// Phase 0 rule 4: every error carries user-facing text.
    #[test]
    fn every_error_has_a_nonempty_message() {
        let all = [
            CoreError::NoClipAtPlayhead {
                track: "V1".into(),
                frame: 0,
            },
            CoreError::InsufficientHandles { shortfall: 3 },
            CoreError::NoSuchTrack("A2".into()),
            CoreError::NoFramerate,
            CoreError::Unconformable {
                source_fps: "23.976".into(),
                timeline_fps: "60".into(),
            },
            CoreError::OfflineMedia {
                path: "/x.mkv".into(),
            },
            CoreError::InvariantViolation("x".into()),
        ];
        for e in &all {
            let msg = e.user_message();
            assert!(!msg.is_empty(), "empty message for {e:?}");
            assert!(!msg.contains('{'), "unformatted message for {e:?}");
        }
    }

    #[test]
    fn notice_carries_class() {
        let e = CoreError::InsufficientHandles { shortfall: 12 };
        let n = Notice::from_error(&e);
        assert_eq!(n.class, ErrorClass::User);
        assert!(n.text.contains("12"));
    }
}
