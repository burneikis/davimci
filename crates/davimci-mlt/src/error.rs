//! Errors from the MLT layer.
//!
//! These stay inside this crate: [`MltError`] converts into the
//! backend-agnostic `BackendError` at the trait boundary, so no caller ever
//! learns that MLT exists (spec §10.1).

use davimci_backend::BackendError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MltError {
    #[error("the MLT framework could not be initialised")]
    Init,

    #[error("MLT has no {service} service for {resource}")]
    NoProducer { service: String, resource: String },

    #[error("MLT has no {service} filter")]
    NoFilter { service: String },

    #[error("MLT has no {service} consumer")]
    NoConsumer { service: String },

    #[error("the playlist operation failed (code {code})")]
    PlaylistOp { code: i32 },

    #[error("the filter could not be attached")]
    AttachFailed,

    #[error("the consumer could not be connected")]
    ConnectFailed,

    #[error("the consumer would not start")]
    ConsumerStart,

    #[error("the event listener could not be registered")]
    ListenFailed,

    #[error("no frame was produced")]
    NoFrame,

    #[error("no image was produced for this frame")]
    NoImage,

    #[error("the frame came back in image format {format} rather than RGBA")]
    WrongFormat { format: i32 },

    #[error("the value {value:?} cannot be passed to MLT")]
    BadString { value: String },
}

impl From<MltError> for BackendError {
    fn from(e: MltError) -> Self {
        let reason = e.to_string();
        match e {
            MltError::NoProducer { resource, .. } => Self::Probe {
                path: resource,
                reason,
            },
            MltError::NoFrame | MltError::NoImage | MltError::WrongFormat { .. } => {
                Self::Decode { frame: 0, reason }
            }
            MltError::Init
            | MltError::NoConsumer { .. }
            | MltError::ConsumerStart
            | MltError::ConnectFailed
            | MltError::ListenFailed => Self::Unavailable { reason },
            MltError::PlaylistOp { .. }
            | MltError::AttachFailed
            | MltError::NoFilter { .. }
            | MltError::BadString { .. } => Self::Projection { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use davimci_core::{Classify, ErrorClass};

    #[test]
    fn mlt_errors_arrive_classified_and_readable() {
        let err: BackendError = MltError::NoFrame.into();
        assert_eq!(err.class(), ErrorClass::Recoverable);
        assert!(err.user_message().ends_with('.') || !err.user_message().is_empty());

        let err: BackendError = MltError::Init.into();
        assert_eq!(err.class(), ErrorClass::Recoverable);
        assert!(
            !err.user_message().contains("Init"),
            "raw Debug output must never reach the status line"
        );
    }
}
