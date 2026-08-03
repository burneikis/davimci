//! What an edit acts on: a time range across a set of tracks (spec 6).
//!
//! A visual selection lives in the key engine, but a `:` command runs in the
//! host - so the shape of "what is selected" has to be a model type both can
//! name. It is deliberately not a list of clips: a selection is a region, and
//! which clips fall in it is a question answered against a timeline, at the
//! moment the command runs.

use crate::clip::Clip;
use crate::id::TrackId;
use crate::time::Frame;
use crate::timeline::Timeline;

/// A half-open time range `[start, end)` across one or more tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub start: Frame,
    pub end: Frame,
    /// The tracks the selection covers, in the order they were selected.
    pub tracks: Vec<TrackId>,
}

impl Selection {
    #[must_use]
    pub fn new(start: Frame, end: Frame, tracks: Vec<TrackId>) -> Self {
        let (start, end) = if end < start {
            (end, start)
        } else {
            (start, end)
        };
        Self { start, end, tracks }
    }

    /// A selection covering nothing: an empty range, or no tracks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.end <= self.start || self.tracks.is_empty()
    }

    /// Every clip that overlaps the selection, paired with the track it is on.
    ///
    /// Overlap, not containment: a selection that clips the tail of a clip
    /// still selects that clip, which is what the user sees on screen. Clips
    /// come back in track order and then in timeline order, so a command
    /// built from this is deterministic.
    #[must_use]
    pub fn clips<'tl>(&self, tl: &'tl Timeline) -> Vec<(TrackId, &'tl Clip)> {
        let mut out = Vec::new();
        if self.is_empty() {
            return out;
        }
        for &track in &self.tracks {
            let Some(t) = tl.track(track) else { continue };
            for clip in t.clips() {
                if clip.end() > self.start && clip.start < self.end {
                    out.push((track, clip));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::fixture;

    fn tl_with(spans: &[(u64, u64)]) -> Timeline {
        let clips: Vec<(u64, u64, &str)> = spans.iter().map(|(s, d)| (*s, *d, "c")).collect();
        fixture(&[("V1", &clips)])
    }

    #[test]
    fn a_reversed_range_normalises() {
        let s = Selection::new(Frame(90), Frame(10), vec![TrackId(1)]);
        assert_eq!((s.start, s.end), (Frame(10), Frame(90)));
    }

    #[test]
    fn a_selection_selects_every_overlapping_clip_not_only_contained_ones() {
        let tl = tl_with(&[(0, 100), (100, 100), (200, 100)]);
        let track = tl.tracks()[0].id;
        // Starts inside clip 0 and ends inside clip 2: all three overlap.
        let s = Selection::new(Frame(50), Frame(250), vec![track]);
        assert_eq!(s.clips(&tl).len(), 3);
        // Touching an edge is not an overlap: [100,200) is clip 1 alone.
        let s = Selection::new(Frame(100), Frame(200), vec![track]);
        let got: Vec<_> = s.clips(&tl).iter().map(|(_, c)| c.start).collect();
        assert_eq!(got, vec![Frame(100)]);
    }

    #[test]
    fn an_empty_selection_selects_nothing() {
        let tl = tl_with(&[(0, 100)]);
        let track = tl.tracks()[0].id;
        assert!(
            Selection::new(Frame(10), Frame(10), vec![track])
                .clips(&tl)
                .is_empty()
        );
        assert!(
            Selection::new(Frame(0), Frame(100), Vec::new())
                .clips(&tl)
                .is_empty()
        );
    }
}
