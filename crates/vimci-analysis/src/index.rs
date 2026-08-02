//! The indexed store behind predicate motions (spec §3.4, §10.2).
//!
//! `]a` must be instant and correct even when zoomed fully out, so it may not
//! scan. Peaks are queried through a max segment tree, which answers "the
//! first hop after here above -2 dB" in O(log n) for a threshold chosen at
//! query time; silence spans are indexed the same way on duration. Scene
//! changes are a sorted list and a binary search.
//!
//! The other half of the contract is honesty about what is not known yet.
//! A track whose analysis is still running answers [`Answer::Pending`], never
//! [`Answer::NoMatch`] - a wrong `NoMatch` would move the playhead and never
//! tell the user it guessed.

use std::collections::BTreeMap;

use vimci_core::{Fps, Frame, TrackId};
use vimci_motion::predicate::{Answer, Predicate, PredicateIndex};
use vimci_motion::target::Direction;

use crate::analysis::Analysis;
use crate::conform::{frame_at_ms, ms_at_frame};

/// A max segment tree supporting "first/last index whose value is at least
/// `x`", for a threshold that is not known until query time.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaxTree {
    n: usize,
    max: Vec<f32>,
}

impl MaxTree {
    #[must_use]
    pub fn new(values: &[f32]) -> Self {
        let n = values.len();
        let mut t = Self {
            n,
            max: vec![f32::NEG_INFINITY; 4 * n.max(1)],
        };
        if n > 0 {
            t.build(1, 0, n - 1, values);
        }
        t
    }

    fn build(&mut self, node: usize, lo: usize, hi: usize, values: &[f32]) {
        if lo == hi {
            self.max[node] = values[lo];
            return;
        }
        let mid = (lo + hi) / 2;
        self.build(2 * node, lo, mid, values);
        self.build(2 * node + 1, mid + 1, hi, values);
        self.max[node] = self.max[2 * node].max(self.max[2 * node + 1]);
    }

    /// First index at or after `from` whose value is >= `threshold`.
    #[must_use]
    pub fn first_at_least(&self, from: usize, threshold: f32) -> Option<usize> {
        if self.n == 0 || from >= self.n {
            return None;
        }
        self.descend(1, 0, self.n - 1, from, threshold, true)
    }

    /// Last index at or before `to` whose value is >= `threshold`.
    #[must_use]
    pub fn last_at_least(&self, to: usize, threshold: f32) -> Option<usize> {
        if self.n == 0 {
            return None;
        }
        self.descend(1, 0, self.n - 1, to.min(self.n - 1), threshold, false)
    }

    /// Walk only the subtrees that can contain a hit, so the cost is the
    /// depth of the tree rather than the length of the range.
    fn descend(
        &self,
        node: usize,
        lo: usize,
        hi: usize,
        bound: usize,
        threshold: f32,
        forward: bool,
    ) -> Option<usize> {
        if self.max[node] < threshold {
            return None;
        }
        if forward && hi < bound {
            return None;
        }
        if !forward && lo > bound {
            return None;
        }
        if lo == hi {
            return Some(lo);
        }
        let mid = (lo + hi) / 2;
        let (first, second) = if forward {
            ((2 * node, lo, mid), (2 * node + 1, mid + 1, hi))
        } else {
            ((2 * node + 1, mid + 1, hi), (2 * node, lo, mid))
        };
        self.descend(first.0, first.1, first.2, bound, threshold, forward)
            .or_else(|| self.descend(second.0, second.1, second.2, bound, threshold, forward))
    }
}

/// What is known about one track's media.
#[derive(Debug, Clone, PartialEq)]
pub enum TrackAnalysis {
    /// Analysis has not finished (or has been invalidated by an edit).
    Pending,
    /// Analysis failed. Editing continues; predicate motions stay honest by
    /// reporting `Pending` rather than a confident wrong answer.
    Failed(String),
    Ready(Box<Indexed>),
}

/// One track's analysis, in query shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Indexed {
    hop_ms: u32,
    peaks: MaxTree,
    silence_starts: Vec<u64>,
    silence_durations: MaxTree,
    scene_changes: Vec<u64>,
}

impl Indexed {
    #[must_use]
    pub fn build(analysis: &Analysis) -> Self {
        let peaks: Vec<f32> = analysis.hops.iter().map(|h| h.peak_db).collect();
        let starts: Vec<u64> = analysis.silence.iter().map(|s| s.start_ms).collect();
        let durations: Vec<f32> = analysis
            .silence
            .iter()
            .map(|s| s.duration_ms() as f32)
            .collect();
        Self {
            hop_ms: analysis.params.hop_ms.max(1),
            peaks: MaxTree::new(&peaks),
            silence_starts: starts,
            silence_durations: MaxTree::new(&durations),
            scene_changes: analysis.scene_changes.clone(),
        }
    }
}

/// The analysis-backed [`PredicateIndex`] (plan.md Phase 5).
#[derive(Debug, Clone, Default)]
pub struct AnalysisIndex {
    fps: Option<Fps>,
    tracks: BTreeMap<TrackId, TrackAnalysis>,
}

impl AnalysisIndex {
    #[must_use]
    pub fn new(fps: Fps) -> Self {
        Self {
            fps: Some(fps),
            tracks: BTreeMap::new(),
        }
    }

    /// Mark a track as analysing. Every track starts here.
    pub fn set_pending(&mut self, track: TrackId) {
        self.tracks.insert(track, TrackAnalysis::Pending);
    }

    pub fn set_failed(&mut self, track: TrackId, reason: impl Into<String>) {
        self.tracks
            .insert(track, TrackAnalysis::Failed(reason.into()));
    }

    /// Publish a finished analysis for a track.
    pub fn insert(&mut self, track: TrackId, analysis: &Analysis) {
        self.tracks.insert(
            track,
            TrackAnalysis::Ready(Box::new(Indexed::build(analysis))),
        );
    }

    /// Invalidate a track: gain or fades changed, so the cached measurements
    /// no longer describe what will be heard (spec §10.2, `:analyze`).
    pub fn invalidate(&mut self, track: TrackId) {
        if self.tracks.contains_key(&track) {
            self.tracks.insert(track, TrackAnalysis::Pending);
        }
    }

    #[must_use]
    pub fn state(&self, track: TrackId) -> Option<&TrackAnalysis> {
        self.tracks.get(&track)
    }

    #[must_use]
    pub fn is_ready(&self, track: TrackId) -> bool {
        matches!(self.tracks.get(&track), Some(TrackAnalysis::Ready(_)))
    }

    fn ready(&self, track: TrackId) -> Option<&Indexed> {
        match self.tracks.get(&track) {
            Some(TrackAnalysis::Ready(i)) => Some(i),
            _ => None,
        }
    }
}

impl PredicateIndex for AnalysisIndex {
    fn find(&self, predicate: &Predicate, from: Frame, dir: Direction) -> Answer {
        let (Some(fps), Some(idx)) = (self.fps, self.ready(predicate.track())) else {
            // Unknown track, unfinished analysis, or a failed job: all three
            // are "not known", and none of them is "not there".
            return Answer::Pending;
        };
        let from_ms = ms_at_frame(from, fps);
        let hit_ms = match predicate {
            Predicate::AudioPeak { threshold_db, .. } => {
                let hop = from_ms / u64::from(idx.hop_ms);
                match dir {
                    Direction::Forward => idx
                        .peaks
                        .first_at_least(hop as usize + 1, *threshold_db)
                        .map(|i| i as u64 * u64::from(idx.hop_ms)),
                    Direction::Backward => hop.checked_sub(1).and_then(|before| {
                        idx.peaks
                            .last_at_least(before as usize, *threshold_db)
                            .map(|i| i as u64 * u64::from(idx.hop_ms))
                    }),
                }
            }
            Predicate::Silence {
                min_duration_ms, ..
            } => {
                let want = *min_duration_ms as f32;
                // Spans are sorted, so the search window is a binary search
                // and the pick within it is another O(log n) descent.
                match dir {
                    Direction::Forward => {
                        let lo = idx.silence_starts.partition_point(|s| *s <= from_ms);
                        idx.silence_durations
                            .first_at_least(lo, want)
                            .and_then(|i| idx.silence_starts.get(i).copied())
                    }
                    Direction::Backward => {
                        let hi = idx.silence_starts.partition_point(|s| *s < from_ms);
                        hi.checked_sub(1)
                            .and_then(|hi| idx.silence_durations.last_at_least(hi, want))
                            .and_then(|i| idx.silence_starts.get(i).copied())
                    }
                }
            }
            Predicate::SceneChange { .. } => match dir {
                Direction::Forward => {
                    let i = idx.scene_changes.partition_point(|s| *s <= from_ms);
                    idx.scene_changes.get(i).copied()
                }
                Direction::Backward => {
                    let i = idx.scene_changes.partition_point(|s| *s < from_ms);
                    i.checked_sub(1)
                        .and_then(|i| idx.scene_changes.get(i).copied())
                }
            },
            // Clip tags are not part of the model yet (they arrive with the
            // Lua API in Phase 7), so there is genuinely nothing to match.
            Predicate::Tagged { .. } => None,
        };
        match hit_ms {
            Some(ms) => {
                let frame = frame_at_ms(ms, fps);
                // A hit must be strictly beyond the origin, or a repeated
                // motion would stand still.
                match dir {
                    Direction::Forward if frame <= from => Answer::Found(Frame(from.get() + 1)),
                    _ => Answer::Found(frame),
                }
            }
            None => Answer::NoMatch,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::analysis::{AnalysisParams, analyze_samples, tests::tone_gaps};

    fn index() -> (AnalysisIndex, TrackId) {
        let a = analyze_samples(&tone_gaps(48_000), 48_000, AnalysisParams::default());
        let mut idx = AnalysisIndex::new(Fps::FPS_60);
        let track = TrackId(2);
        idx.insert(track, &a);
        (idx, track)
    }

    fn peak(track: TrackId, threshold_db: f32) -> Predicate {
        Predicate::AudioPeak {
            track,
            threshold_db,
        }
    }

    #[test]
    fn a_peak_search_lands_on_the_tone() {
        let (idx, track) = index();
        // Tone starts at 1.000s = frame 60 at 60fps.
        assert_eq!(
            idx.find(&peak(track, -12.0), Frame::ZERO, Direction::Forward),
            Answer::Found(Frame(60))
        );
        // ... and the second burst at 3.000s = frame 180.
        assert_eq!(
            idx.find(&peak(track, -12.0), Frame(150), Direction::Forward),
            Answer::Found(Frame(180))
        );
        // Backwards from the end finds the last loud hop, at 3.990s.
        assert_eq!(
            idx.find(&peak(track, -12.0), Frame(299), Direction::Backward),
            Answer::Found(Frame(239))
        );
    }

    #[test]
    fn a_threshold_nothing_reaches_is_no_match_not_pending() {
        let (idx, track) = index();
        assert_eq!(
            idx.find(&peak(track, 0.0), Frame::ZERO, Direction::Forward),
            Answer::NoMatch
        );
    }

    #[test]
    fn silence_search_respects_the_minimum_duration() {
        let (idx, track) = index();
        let long = Predicate::Silence {
            track,
            min_duration_ms: 500,
            threshold_db: -50.0,
        };
        // Silence spans are 0-1s, 2-3s, 4-5s: the next one after frame 60
        // (1.0s) starts at 2.0s = frame 120.
        assert_eq!(
            idx.find(&long, Frame(60), Direction::Forward),
            Answer::Found(Frame(120))
        );
        assert_eq!(
            idx.find(&long, Frame(130), Direction::Backward),
            Answer::Found(Frame(120))
        );
        let impossible = Predicate::Silence {
            track,
            min_duration_ms: 10_000,
            threshold_db: -50.0,
        };
        assert_eq!(
            idx.find(&impossible, Frame::ZERO, Direction::Forward),
            Answer::NoMatch
        );
    }

    #[test]
    fn scene_changes_are_found_in_both_directions() {
        let mut a = analyze_samples(&[], 48_000, AnalysisParams::default());
        a.scene_changes = vec![2000, 4000];
        let mut idx = AnalysisIndex::new(Fps::FPS_60);
        idx.insert(TrackId(1), &a);
        let p = Predicate::SceneChange { track: TrackId(1) };
        assert_eq!(
            idx.find(&p, Frame::ZERO, Direction::Forward),
            Answer::Found(Frame(120))
        );
        assert_eq!(
            idx.find(&p, Frame(120), Direction::Forward),
            Answer::Found(Frame(240))
        );
        assert_eq!(
            idx.find(&p, Frame(240), Direction::Backward),
            Answer::Found(Frame(120))
        );
        assert_eq!(
            idx.find(&p, Frame::ZERO, Direction::Backward),
            Answer::NoMatch
        );
    }

    /// plan.md Phase 5: editing during an in-flight job must leave predicate
    /// motions `Pending`, never stale or wrong.
    #[test]
    fn an_unfinished_or_failed_track_is_pending_not_no_match() {
        let (mut idx, ready) = index();
        idx.set_pending(TrackId(9));
        assert_eq!(
            idx.find(&peak(TrackId(9), -12.0), Frame::ZERO, Direction::Forward),
            Answer::Pending
        );
        idx.set_failed(TrackId(9), "decode error");
        assert_eq!(
            idx.find(&peak(TrackId(9), -12.0), Frame::ZERO, Direction::Forward),
            Answer::Pending
        );
        // A track nobody ever queued is unknown, which is also not "absent".
        assert_eq!(
            idx.find(&peak(TrackId(77), -12.0), Frame::ZERO, Direction::Forward),
            Answer::Pending
        );
        // The ready track keeps answering while its neighbour is analysing.
        assert!(matches!(
            idx.find(&peak(ready, -12.0), Frame::ZERO, Direction::Forward),
            Answer::Found(_)
        ));
    }

    #[test]
    fn invalidating_a_track_makes_it_pending_again() {
        let (mut idx, track) = index();
        assert!(idx.is_ready(track));
        idx.invalidate(track);
        assert!(!idx.is_ready(track));
        assert_eq!(
            idx.find(&peak(track, -12.0), Frame::ZERO, Direction::Forward),
            Answer::Pending
        );
    }

    #[test]
    fn tags_are_not_modelled_yet_and_say_so_by_matching_nothing() {
        let (idx, track) = index();
        assert_eq!(
            idx.find(
                &Predicate::Tagged {
                    track,
                    tag: "keep".into()
                },
                Frame::ZERO,
                Direction::Forward
            ),
            Answer::NoMatch
        );
    }

    #[test]
    fn the_max_tree_agrees_with_a_linear_scan() {
        let values: Vec<f32> = (0..97).map(|i| ((i * 37) % 91) as f32 - 45.0).collect();
        let tree = MaxTree::new(&values);
        for threshold in [-50.0f32, -10.0, 0.0, 20.0, 44.0, 45.0, 100.0] {
            for from in 0..values.len() {
                let want = values
                    .iter()
                    .enumerate()
                    .skip(from)
                    .find(|(_, v)| **v >= threshold)
                    .map(|(i, _)| i);
                assert_eq!(tree.first_at_least(from, threshold), want, "from {from}");
                let want_back = values
                    .iter()
                    .enumerate()
                    .take(from + 1)
                    .rev()
                    .find(|(_, v)| **v >= threshold)
                    .map(|(i, _)| i);
                assert_eq!(tree.last_at_least(from, threshold), want_back, "to {from}");
            }
        }
    }

    #[test]
    fn an_empty_tree_answers_without_panicking() {
        let tree = MaxTree::new(&[]);
        assert_eq!(tree.first_at_least(0, 0.0), None);
        assert_eq!(tree.last_at_least(5, 0.0), None);
    }
}
