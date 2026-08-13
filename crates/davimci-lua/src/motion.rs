//! The data a user-defined motion is allowed to see.
//!
//! A registered motion is a pure query - it is handed a snapshot and returns
//! a frame - so a plugin cannot move the playhead behind the editor's back,
//! and a motion is testable with no timeline, no media, and no backend.

use std::collections::BTreeMap;

/// One analysis sample, at the analysis hop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub frame: u64,
    pub rms_db: f64,
    pub peak_db: f64,
}

/// What one track contributes to a motion query.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackData {
    /// `"video"`, `"audio"`, `"text"`, `"overlay"` - matched against the
    /// `type` field of a `find_next` query.
    pub kind: String,
    /// Sorted by frame.
    pub samples: Vec<Sample>,
    /// Clip start/end frames, sorted.
    pub clip_bounds: Vec<u64>,
    /// Frames the analysis called a scene change, sorted. Empty for a track
    /// nothing detected scenes in; what counts as a cut worth jumping to is
    /// a plugin's policy, not the model's.
    pub scene_changes: Vec<u64>,
    /// Whether analysis for this track has finished. A motion over an
    /// unanalysed track reports "not yet" rather than a wrong frame.
    pub analysed: bool,
}

/// The snapshot a motion runs against.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MotionEnv {
    pub playhead: u64,
    pub focused_track: String,
    pub tracks: BTreeMap<String, TrackData>,
}

impl MotionEnv {
    #[must_use]
    pub fn new(playhead: u64, focused_track: impl Into<String>) -> Self {
        Self {
            playhead,
            focused_track: focused_track.into(),
            tracks: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_track(mut self, name: impl Into<String>, data: TrackData) -> Self {
        self.tracks.insert(name.into(), data);
        self
    }
}

/// What a registered motion answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionAnswer {
    Found(u64),
    NoMatch,
    /// The track the motion asked about is still being analysed. This is the
    /// same honesty rule as `davimci_motion::Answer::Pending`: never guess.
    Pending,
}
