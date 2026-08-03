//! Predicate motions (spec 3.4).
//!
//! These are the motions that ask a question about the *media* rather than
//! the timeline structure: "next audio peak above -2 dB", "next silence
//! longer than 500 ms". The answers come from the Phase 5 analysis index,
//! which runs in the background, so the interface has three outcomes rather
//! than two: found, definitely-not-found, and not-yet-known.

use davimci_core::{Frame, TrackId};

use crate::target::Direction;

/// A media condition to search for.
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    /// `next_audio_peak(track, threshold_db)`
    AudioPeak { track: TrackId, threshold_db: f32 },
    /// `next_silence(track, min_duration_ms, threshold_db)`
    Silence {
        track: TrackId,
        min_duration_ms: u32,
        threshold_db: f32,
    },
    /// `next_scene_change(track)`
    SceneChange { track: TrackId },
    /// `next_clip_tagged(tag)`
    Tagged { track: TrackId, tag: String },
}

impl Predicate {
    #[must_use]
    pub fn track(&self) -> TrackId {
        match self {
            Self::AudioPeak { track, .. }
            | Self::Silence { track, .. }
            | Self::SceneChange { track }
            | Self::Tagged { track, .. } => *track,
        }
    }
}

/// The answer to a predicate query.
///
/// `Pending` is load-bearing: a partially analysed track must never return
/// `NoMatch`, because the caller would move the playhead to the wrong place
/// and never learn it was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Found(Frame),
    NoMatch,
    Pending,
}

/// The analysis-backed index a predicate motion queries.
///
/// Implemented by `davimci-analysis` in Phase 5. `davimci-motion` depends only on
/// this trait so the model layer stays free of media and I/O.
pub trait PredicateIndex: std::fmt::Debug {
    /// First match strictly beyond `from` in `dir`.
    fn find(&self, predicate: &Predicate, from: Frame, dir: Direction) -> Answer;
}

/// The index used before analysis exists: everything is `Pending`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAnalysis;

impl PredicateIndex for NoAnalysis {
    fn find(&self, _predicate: &Predicate, _from: Frame, _dir: Direction) -> Answer {
        Answer::Pending
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A fixed set of hit frames per track, for motion tests.
    #[derive(Debug, Default)]
    pub struct StubIndex {
        pub hits: Vec<(TrackId, Frame)>,
        pub pending: bool,
    }

    impl PredicateIndex for StubIndex {
        fn find(&self, predicate: &Predicate, from: Frame, dir: Direction) -> Answer {
            if self.pending {
                return Answer::Pending;
            }
            let track = predicate.track();
            let mut hits: Vec<Frame> = self
                .hits
                .iter()
                .filter(|(t, _)| *t == track)
                .map(|(_, f)| *f)
                .collect();
            hits.sort_unstable();
            let found = match dir {
                Direction::Forward => hits.into_iter().find(|f| *f > from),
                Direction::Backward => hits.into_iter().rev().find(|f| *f < from),
            };
            found.map_or(Answer::NoMatch, Answer::Found)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::StubIndex;
    use super::*;

    fn peak() -> Predicate {
        Predicate::AudioPeak {
            track: TrackId(1),
            threshold_db: -2.0,
        }
    }

    #[test]
    fn without_analysis_every_query_is_pending() {
        assert_eq!(
            NoAnalysis.find(&peak(), Frame(0), Direction::Forward),
            Answer::Pending
        );
    }

    #[test]
    fn the_stub_finds_strictly_beyond_the_origin() {
        let idx = StubIndex {
            hits: vec![(TrackId(1), Frame(50)), (TrackId(1), Frame(10))],
            pending: false,
        };
        assert_eq!(
            idx.find(&peak(), Frame(10), Direction::Forward),
            Answer::Found(Frame(50))
        );
        assert_eq!(
            idx.find(&peak(), Frame(50), Direction::Backward),
            Answer::Found(Frame(10))
        );
        assert_eq!(
            idx.find(&peak(), Frame(50), Direction::Forward),
            Answer::NoMatch
        );
    }

    #[test]
    fn a_predicate_reports_the_track_it_searches() {
        assert_eq!(peak().track(), TrackId(1));
        assert_eq!(
            Predicate::Silence {
                track: TrackId(3),
                min_duration_ms: 500,
                threshold_db: -40.0,
            }
            .track(),
            TrackId(3)
        );
        assert_eq!(
            Predicate::SceneChange { track: TrackId(4) }.track(),
            TrackId(4)
        );
        assert_eq!(
            Predicate::Tagged {
                track: TrackId(5),
                tag: "keep".into()
            }
            .track(),
            TrackId(5)
        );
    }
}
