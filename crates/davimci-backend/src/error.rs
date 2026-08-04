//! Backend errors, classified by recovery policy.

use davimci_core::{Classify, ErrorClass};

/// Errors raised by a [`RenderBackend`](crate::RenderBackend).
///
/// Every variant carries a complete user-facing sentence, and every variant
/// maps onto exactly one Phase 0 recovery policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackendError {
    #[error("could not read media at {path}: {reason}")]
    Probe { path: String, reason: String },

    #[error("source media is offline: {path}")]
    Offline { path: String },

    #[error("could not seek to frame {frame}")]
    Seek { frame: u64 },

    #[error("could not decode frame {frame}: {reason}")]
    Decode { frame: u64, reason: String },

    #[error("the timeline could not be projected onto the render graph: {reason}")]
    Projection { reason: String },

    #[error("preview is not running")]
    PreviewNotRunning,

    #[error("preview is already running")]
    PreviewAlreadyRunning,

    #[error("the render failed: {reason}")]
    Render { reason: String },

    #[error("the render backend is unavailable: {reason}")]
    Unavailable { reason: String },

    #[error("{what} is not supported by this render backend")]
    Unsupported { what: String },
}

impl Classify for BackendError {
    fn class(&self) -> ErrorClass {
        match self {
            // The user pointed at media that is gone; the project stays open.
            Self::Offline { .. } | Self::Probe { .. } => ErrorClass::OfflineMedia,
            // Local degradation: black frame, failed render, keep editing.
            Self::Seek { .. }
            | Self::Decode { .. }
            | Self::Render { .. }
            | Self::Unavailable { .. } => ErrorClass::Recoverable,
            // The user asked for something this backend cannot do.
            Self::Unsupported { .. } | Self::PreviewNotRunning | Self::PreviewAlreadyRunning => {
                ErrorClass::User
            }
            // A timeline we cannot project is a timeline we do not understand.
            Self::Projection { .. } => ErrorClass::Corruption,
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

/// Convenience alias for backend results.
pub type Result<T> = std::result::Result<T, BackendError>;
