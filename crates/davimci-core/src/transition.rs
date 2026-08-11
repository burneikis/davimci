//! Transitions: the overlap between two abutting clips.
//!
//! The timeline model has no overlapping clips - that invariant is what every
//! motion, ripple, and projection rests on - so a transition is not a third
//! clip laid over two others. It is a property of the *incoming* clip: a
//! named type and a duration, attached to the cut at that clip's start.
//!
//! No transition type is core. The model stores whatever name it was given
//! and never checks it against a catalogue: naming what an overlap looks like
//! is a plugin's job, and a name nothing registered still round-trips through
//! save and load unchanged.
//!
//! The overlap is materialised at render time out of handle frames. A
//! transition of `d` frames on a cut is centred on it: the outgoing clip runs
//! [`Transition::tail`] frames past its out-point, the incoming clip starts
//! [`Transition::head`] frames before its in-point, and the two are composited
//! over the region `[cut - head, cut + tail)`. Nothing on the timeline moves,
//! so creating or deleting a transition never ripples.

use serde::{Deserialize, Serialize};

use crate::time::Frame;

/// The default transition length in frames.
///
/// A length is core because the model owns the overlap; the *type* is not,
/// so nothing here names one.
pub const DEFAULT_TRANSITION_FRAMES: u64 = 12;

/// A transition attached to the head of a clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// Registry name. Every type, including the plainest cross-fade, is
    /// registered by a plugin, so the model stores the name rather than an
    /// enum and never asserts that any particular one exists.
    pub kind: String,
    /// Length of the overlap in timeline frames. Always non-zero.
    pub duration: Frame,
}

impl Transition {
    #[must_use]
    pub fn new(kind: impl Into<String>, duration: Frame) -> Self {
        Self {
            kind: kind.into(),
            duration,
        }
    }

    /// `kind` at the default length.
    #[must_use]
    pub fn of(kind: impl Into<String>) -> Self {
        Self::new(kind, Frame(DEFAULT_TRANSITION_FRAMES))
    }

    /// Frames the overlap reaches *before* the cut, taken from the incoming
    /// clip's head handle.
    #[must_use]
    pub fn head(&self) -> u64 {
        self.duration.get() / 2
    }

    /// Frames the overlap reaches *after* the cut, taken from the outgoing
    /// clip's tail handle. The odd frame goes here, so `head + tail == d`.
    #[must_use]
    pub fn tail(&self) -> u64 {
        self.duration.get() - self.head()
    }

    /// The half-open timeline range the overlap covers, given its cut.
    #[must_use]
    pub fn span(&self, cut: Frame) -> (Frame, Frame) {
        (
            Frame(cut.get().saturating_sub(self.head())),
            Frame(cut.get() + self.tail()),
        )
    }

    /// Whether the overlap covers `frame`.
    #[must_use]
    pub fn covers(&self, cut: Frame, frame: Frame) -> bool {
        let (start, end) = self.span(cut);
        frame >= start && frame < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_odd_duration_puts_the_extra_frame_after_the_cut() {
        let t = Transition::new("dissolve", Frame(7));
        assert_eq!((t.head(), t.tail()), (3, 4));
        assert_eq!(t.head() + t.tail(), 7);
        assert_eq!(t.span(Frame(100)), (Frame(97), Frame(104)));
    }

    #[test]
    fn a_type_at_the_default_length_is_twelve_frames() {
        let t = Transition::of("dissolve");
        assert_eq!(t.kind, "dissolve");
        assert_eq!(t.duration, Frame(DEFAULT_TRANSITION_FRAMES));
        assert_eq!((t.head(), t.tail()), (6, 6));
    }

    #[test]
    fn the_span_is_half_open_around_the_cut() {
        let t = Transition::new("dissolve", Frame(4));
        assert!(t.covers(Frame(50), Frame(48)));
        assert!(t.covers(Frame(50), Frame(51)));
        assert!(!t.covers(Frame(50), Frame(52)));
        assert!(!t.covers(Frame(50), Frame(47)));
    }

    /// A transition at frame zero has no room before the cut, and clamping
    /// there must not wrap the range round to a huge span.
    #[test]
    fn a_span_at_frame_zero_clamps_rather_than_wrapping() {
        let t = Transition::new("dissolve", Frame(12));
        assert_eq!(t.span(Frame(2)), (Frame(0), Frame(8)));
    }
}
