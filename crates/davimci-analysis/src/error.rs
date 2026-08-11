//! Analysis and import errors, classified by recovery policy.
//!
//! Import is the one place where all four error classes meet: a missing file
//! is offline media, an unsupported container is a user error, a crashed
//! analysis job is recoverable, and a corrupt cache is recoverable *by
//! recomputation* - never by giving up and never by panicking.

use davimci_core::{Classify, CoreError, ErrorClass};

/// Errors from probing, importing, analysing, caching, and proxying media.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnalysisError {
    #[error("cannot read {path}: the file is missing or unreadable")]
    MediaOffline { path: String },

    #[error("could not probe {path}: {reason}")]
    ProbeFailed { path: String, reason: String },

    #[error("{path} has no streams davimci can import")]
    NoImportableStreams { path: String },

    #[error("{path} is not supported: {reason}")]
    Unsupported { path: String, reason: String },

    #[error("analysis of {path} failed: {reason}")]
    AnalysisFailed { path: String, reason: String },

    #[error("the analysis job was cancelled")]
    Cancelled,

    #[error("could not write the analysis cache: {reason}")]
    CacheUnwritable { reason: String },

    #[error("{tool} is not installed, so {what} is unavailable")]
    ToolMissing {
        tool: &'static str,
        what: &'static str,
    },

    #[error("'{name}' is not a decoder davimci knows; use auto, none, cuda or vaapi")]
    UnknownAccel { name: String },

    #[error("clip {clip} would render from the proxy {path}; export needs the original")]
    ProxyInExport { clip: String, path: String },

    #[error(transparent)]
    Core(#[from] CoreError),

    /// The import command was rejected. Its class - and so its recovery
    /// policy - is the command layer's, not ours.
    #[error(transparent)]
    Command(#[from] davimci_cmd::CmdError),
}

impl Classify for AnalysisError {
    fn class(&self) -> ErrorClass {
        match self {
            Self::MediaOffline { .. } => ErrorClass::OfflineMedia,
            Self::NoImportableStreams { .. }
            | Self::Unsupported { .. }
            | Self::UnknownAccel { .. }
            | Self::ProxyInExport { .. } => ErrorClass::User,
            Self::ProbeFailed { .. }
            | Self::AnalysisFailed { .. }
            | Self::Cancelled
            | Self::CacheUnwritable { .. }
            | Self::ToolMissing { .. } => ErrorClass::Recoverable,
            Self::Core(e) => e.class(),
            Self::Command(e) => e.class(),
        }
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

impl AnalysisError {
    /// An I/O failure against `path`, classified by whether the file is there.
    pub(crate) fn io(path: &str, err: &std::io::Error) -> Self {
        if err.kind() == std::io::ErrorKind::NotFound {
            Self::MediaOffline {
                path: path.to_string(),
            }
        } else {
            Self::ProbeFailed {
                path: path.to_string(),
                reason: err.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_is_represented_and_readable() {
        let all = [
            AnalysisError::MediaOffline {
                path: "/gone.mkv".into(),
            },
            AnalysisError::ProbeFailed {
                path: "/x.mkv".into(),
                reason: "ffprobe exited 1".into(),
            },
            AnalysisError::NoImportableStreams {
                path: "/x.txt".into(),
            },
            AnalysisError::Unsupported {
                path: "/x.mkv".into(),
                reason: "variable frame rate".into(),
            },
            AnalysisError::AnalysisFailed {
                path: "/x.mkv".into(),
                reason: "decode error".into(),
            },
            AnalysisError::Cancelled,
            AnalysisError::CacheUnwritable {
                reason: "permission denied".into(),
            },
            AnalysisError::ToolMissing {
                tool: "ffprobe",
                what: "media import",
            },
            AnalysisError::ProxyInExport {
                clip: "c1".into(),
                path: "/proxy.mov".into(),
            },
            AnalysisError::Core(CoreError::ZeroDuration),
            AnalysisError::Command(davimci_cmd::CmdError::NothingToUndo),
        ];
        for e in &all {
            let msg = e.user_message();
            assert!(!msg.is_empty(), "empty message for {e:?}");
            assert!(!msg.contains('{'), "unformatted message for {e:?}");
        }
        assert_eq!(all[0].class(), ErrorClass::OfflineMedia);
        assert_eq!(all[2].class(), ErrorClass::User);
        assert_eq!(all[5].class(), ErrorClass::Recoverable);
    }

    #[test]
    fn a_missing_file_is_offline_media_not_a_probe_failure() {
        let e = AnalysisError::io(
            "/gone.mkv",
            &std::io::Error::new(std::io::ErrorKind::NotFound, "nope"),
        );
        assert_eq!(e.class(), ErrorClass::OfflineMedia);
        let e = AnalysisError::io(
            "/locked.mkv",
            &std::io::Error::new(std::io::ErrorKind::PermissionDenied, "nope"),
        );
        assert_eq!(e.class(), ErrorClass::Recoverable);
    }
}
