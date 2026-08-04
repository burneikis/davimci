//! Motion errors.
//!
//! Every failure here is a *user* error: a motion that cannot land anywhere
//! is rejected with a status-line sentence and mutates nothing.

use davimci_core::{Classify, ErrorClass};

/// A motion or text object that could not resolve to a target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MotionError {
    #[error("there is no jump point in that direction")]
    NoJumpPoint,

    #[error("there is no clip boundary in that direction")]
    NoBoundary,

    #[error("there is no clip under the playhead on track {track}")]
    NoClipUnderPlayhead { track: String },

    #[error("there is no marker in that direction")]
    NoMarker,

    #[error("mark '{0}' is not set")]
    NoSuchMark(char),

    #[error("track {0} does not exist")]
    NoSuchTrack(String),

    #[error("there is no track in that direction")]
    NoTrackThere,

    #[error("there is no matching edit point at frame {frame}")]
    NoMatchingEdit { frame: u64 },

    #[error("no visual selection to use as a segment")]
    NoSegment,

    #[error("nothing on this track matches that condition")]
    NoPredicateMatch,

    #[error("the text object '{0}' is defined in config and only the editor can resolve it")]
    UnresolvedObject(char),
}

impl Classify for MotionError {
    fn class(&self) -> ErrorClass {
        ErrorClass::User
    }

    fn user_message(&self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_carries_a_formatted_sentence() {
        let all = [
            MotionError::NoJumpPoint,
            MotionError::NoBoundary,
            MotionError::NoClipUnderPlayhead { track: "V1".into() },
            MotionError::NoMarker,
            MotionError::NoSuchMark('a'),
            MotionError::NoSuchTrack("A9".into()),
            MotionError::NoTrackThere,
            MotionError::NoMatchingEdit { frame: 12 },
            MotionError::NoSegment,
            MotionError::NoPredicateMatch,
        ];
        for e in &all {
            assert_eq!(e.class(), ErrorClass::User);
            let msg = e.user_message();
            assert!(!msg.is_empty() && !msg.contains('{'), "bad message: {msg}");
        }
    }
}
