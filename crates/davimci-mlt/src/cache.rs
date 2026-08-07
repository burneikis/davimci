//! A bounded cache of decoded preview frames.
//!
//! Stepping *forward* through a file is cheap: the decoder is already there.
//! Stepping *backward* is not - a seek to `n - 1` throws away the decoder
//! state and re-decodes from the preceding keyframe, so walking back through
//! a GOP costs a whole GOP per frame. The fix has two halves, and this module
//! is the first:
//!
//! 1. remember frames that have been decoded, so revisiting one is free;
//! 2. on a backward step, decode the *run* leading up to the target in one
//!    pass (see `MltBackend::frame_at`), so the following steps are hits.
//!
//! The cache is bounded in bytes rather than frames, because a frame is
//! anywhere from a few kilobytes at quarter scale to 8 MB at full 1080p, and
//! a count-based bound would mean either wasted memory or a useless cache
//! depending on which. Eviction is oldest-first: the run just decoded is the
//! run about to be walked, so it must be the last thing thrown away.
//!
//! Entries carry the scale they were decoded at, because thumbnails are
//! pulled at quarter scale between preview steps: a cache that held one scale
//! would throw the backstep run away every time a strip filled in, and the
//! next backward step would pay a whole GOP.

use std::collections::VecDeque;

use davimci_backend::{PreviewScale, VideoFrame};
use davimci_core::Frame;

/// Default budget: enough for roughly a second of 1080p RGBA, which is the
/// span a user actually steps back and forth over.
pub(crate) const DEFAULT_BUDGET_BYTES: usize = 96 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct FrameCache {
    /// Oldest first, so eviction is a `pop_front`.
    entries: VecDeque<(PreviewScale, VideoFrame)>,
    bytes: usize,
    budget: usize,
}

impl Default for FrameCache {
    fn default() -> Self {
        Self::with_budget(DEFAULT_BUDGET_BYTES)
    }
}

impl FrameCache {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            budget,
        }
    }

    /// Drop everything. Called whenever the graph changes: a cached frame of
    /// an edited timeline is a picture of a timeline that no longer exists.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The cached frame at `at`, if one was decoded at `scale`.
    pub(crate) fn get(&self, at: Frame, scale: PreviewScale) -> Option<&VideoFrame> {
        self.entries
            .iter()
            .find(|(s, f)| *s == scale && f.position == at)
            .map(|(_, f)| f)
    }

    /// Take ownership of a decoded frame. Frames at two scales are not
    /// interchangeable, so the scale is part of the key rather than a reason
    /// to discard everything held at another one.
    pub(crate) fn insert(&mut self, scale: PreviewScale, frame: VideoFrame) {
        if self.get(frame.position, scale).is_some() {
            return;
        }
        let size = frame.rgba.len();
        // A single frame larger than the whole budget is not cacheable; it is
        // dropped rather than allowed to evict itself into an empty cache.
        if size > self.budget {
            return;
        }
        self.bytes = self.bytes.saturating_add(size);
        self.entries.push_back((scale, frame));
        while self.bytes > self.budget {
            match self.entries.pop_front() {
                Some((_, old)) => self.bytes = self.bytes.saturating_sub(old.rgba.len()),
                None => break,
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_core::Resolution;

    fn frame(at: u64) -> VideoFrame {
        VideoFrame::black(
            Frame(at),
            Resolution {
                width: 4,
                height: 4,
            },
        )
    }

    fn frame_bytes() -> usize {
        frame(0).rgba.len()
    }

    #[test]
    fn a_cached_frame_comes_back_at_the_same_scale_and_not_another() {
        let mut c = FrameCache::default();
        c.insert(PreviewScale::Full, frame(7));
        assert_eq!(
            c.get(Frame(7), PreviewScale::Full).unwrap().position,
            Frame(7)
        );
        assert!(c.get(Frame(7), PreviewScale::Half).is_none());
        assert!(c.get(Frame(8), PreviewScale::Full).is_none());
    }

    #[test]
    fn two_scales_are_held_apart_rather_than_mixed_or_discarded() {
        let mut c = FrameCache::default();
        c.insert(PreviewScale::Full, frame(1));
        c.insert(PreviewScale::Half, frame(2));
        assert!(c.get(Frame(1), PreviewScale::Half).is_none());
        assert!(c.get(Frame(2), PreviewScale::Full).is_none());
        assert!(c.get(Frame(1), PreviewScale::Full).is_some());
        assert!(c.get(Frame(2), PreviewScale::Half).is_some());
        assert_eq!(c.len(), 2);
    }

    /// Regression: a quarter-scale thumbnail pull between two preview steps
    /// used to clear the cache, so the next backward step paid a whole GOP.
    #[test]
    fn a_thumbnail_pull_does_not_evict_the_preview_run() {
        let mut c = FrameCache::default();
        for i in 0..12 {
            c.insert(PreviewScale::Full, frame(i));
        }
        c.insert(PreviewScale::Quarter, frame(400));
        for i in 0..12 {
            assert!(c.get(Frame(i), PreviewScale::Full).is_some(), "lost {i}");
        }
    }

    #[test]
    fn the_budget_evicts_oldest_first_and_keeps_the_newest_run() {
        let mut c = FrameCache::with_budget(frame_bytes() * 3);
        for i in 0..5 {
            c.insert(PreviewScale::Full, frame(i));
        }
        assert_eq!(c.len(), 3);
        assert!(c.get(Frame(0), PreviewScale::Full).is_none());
        assert!(c.get(Frame(1), PreviewScale::Full).is_none());
        for i in 2..5 {
            assert!(c.get(Frame(i), PreviewScale::Full).is_some(), "evicted {i}");
        }
    }

    #[test]
    fn inserting_the_same_frame_twice_does_not_double_count_it() {
        let mut c = FrameCache::with_budget(frame_bytes() * 2);
        c.insert(PreviewScale::Full, frame(3));
        c.insert(PreviewScale::Full, frame(3));
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes, frame_bytes());
    }

    #[test]
    fn a_frame_bigger_than_the_budget_is_refused_not_stored_alone() {
        let mut c = FrameCache::with_budget(4);
        c.insert(PreviewScale::Full, frame(0));
        assert_eq!(c.len(), 0);
        assert_eq!(c.bytes, 0);
    }

    #[test]
    fn clearing_frees_everything_it_was_holding() {
        let mut c = FrameCache::default();
        c.insert(PreviewScale::Full, frame(0));
        c.clear();
        assert_eq!(c.len(), 0);
        assert_eq!(c.bytes, 0);
        assert!(c.get(Frame(0), PreviewScale::Full).is_none());
    }
}
