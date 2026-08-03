//! Transitions: the overlap between two abutting clips (spec 6.2).
//!
//! The timeline model has no overlapping clips - that invariant is what every
//! motion, ripple, and projection rests on - so a transition is not a third
//! clip laid over two others. It is a property of the *incoming* clip: a
//! named type and a duration, attached to the cut at that clip's start.
//!
//! The overlap is materialised at render time out of handle frames. A
//! transition of `d` frames on a cut is centred on it: the outgoing clip runs
//! [`Transition::tail`] frames past its out-point, the incoming clip starts
//! [`Transition::head`] frames before its in-point, and the two are composited
//! over the region `[cut - head, cut + tail)`. Nothing on the timeline moves,
//! so creating or deleting a transition never ripples.

use serde::{Deserialize, Serialize};

use crate::time::Frame;

/// The transition `gx` creates when the user names nothing (spec 6.2).
pub const DEFAULT_TRANSITION: &str = "dissolve";

/// The default transition length in frames (spec 6.2).
pub const DEFAULT_TRANSITION_FRAMES: u64 = 12;

/// A transition attached to the head of a clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// Registry name, e.g. `dissolve`. Types are extensible from Lua
    /// (spec 6.2), so the model stores the name rather than an enum.
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

    /// A default-length dissolve.
    #[must_use]
    pub fn dissolve() -> Self {
        Self::new(DEFAULT_TRANSITION, Frame(DEFAULT_TRANSITION_FRAMES))
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
    fn the_default_is_a_twelve_frame_dissolve() {
        let t = Transition::dissolve();
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
