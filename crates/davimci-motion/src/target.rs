//! What a motion or text object resolves to.

use davimci_core::{Frame, TrackId};

/// A half-open span of timeline frames, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Frame,
    pub end: Frame,
}

impl TimeRange {
    #[must_use]
    pub fn new(start: Frame, end: Frame) -> Self {
        if end < start {
            Self {
                start: end,
                end: start,
            }
        } else {
            Self { start, end }
        }
    }

    #[must_use]
    pub fn len(self) -> Frame {
        Frame(self.end.get() - self.start.get())
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[must_use]
    pub fn contains(self, frame: Frame) -> bool {
        frame >= self.start && frame < self.end
    }
}

/// Which tracks an operation touches.
///
/// Track objects produce this: `it` yields the focused track alone, `at`
/// yields the focused track plus every track its link group reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    tracks: Vec<TrackId>,
}

impl Scope {
    /// Build a scope, deduplicated and in the order given.
    #[must_use]
    pub fn new(tracks: impl IntoIterator<Item = TrackId>) -> Self {
        let mut out: Vec<TrackId> = Vec::new();
        for t in tracks {
            if !out.contains(&t) {
                out.push(t);
            }
        }
        Self { tracks: out }
    }

    #[must_use]
    pub fn single(track: TrackId) -> Self {
        Self {
            tracks: vec![track],
        }
    }

    #[must_use]
    pub fn tracks(&self) -> &[TrackId] {
        &self.tracks
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    #[must_use]
    pub fn contains(&self, track: TrackId) -> bool {
        self.tracks.contains(&track)
    }
}

/// A playhead position: a frame plus the focused track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub frame: Frame,
    pub track: TrackId,
}

/// The result of resolving a motion or text object.
///
/// `Pending` exists because predicate motions are backed by the
/// analysis index: when analysis has not finished, the honest answer is "not
/// yet", never a stale or guessed frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A new playhead position.
    Position(Position),
    /// A range plus the tracks it applies to.
    Range(TimeRange, Scope),
    /// Analysis is still running; the caller should retry, not guess.
    Pending,
}

impl Resolved {
    /// The frame a motion landed on, for motions that produce a position.
    #[must_use]
    pub fn frame(&self) -> Option<Frame> {
        match self {
            Self::Position(p) => Some(p.frame),
            Self::Range(r, _) => Some(r.start),
            Self::Pending => None,
        }
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Which way a directional motion goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Backward,
    Forward,
}

impl Direction {
    #[must_use]
    pub fn is_forward(self) -> bool {
        matches!(self, Self::Forward)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reversed_range_is_normalised() {
        let r = TimeRange::new(Frame(90), Frame(10));
        assert_eq!((r.start, r.end), (Frame(10), Frame(90)));
        assert_eq!(r.len(), Frame(80));
        assert!(!r.is_empty());
        assert!(r.contains(Frame(10)));
        assert!(!r.contains(Frame(90)));
    }

    #[test]
    fn a_scope_deduplicates_and_keeps_order() {
        let s = Scope::new([TrackId(2), TrackId(1), TrackId(2)]);
        assert_eq!(s.tracks(), [TrackId(2), TrackId(1)]);
        assert_eq!(s.len(), 2);
        assert!(!s.is_empty());
        assert!(s.contains(TrackId(1)));
    }
}
