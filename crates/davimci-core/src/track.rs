//! Tracks: ordered, non-overlapping sequences of clips (spec §5).

use serde::{Deserialize, Serialize};

use crate::clip::Clip;
use crate::error::CoreError;
use crate::id::{ClipId, TrackId};
use crate::time::Frame;
use crate::transition::Transition;

/// Track types from spec §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
    Text,
    Overlay,
}

impl TrackKind {
    #[must_use]
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Video => "V",
            Self::Audio => "A",
            Self::Text => "T",
            Self::Overlay => "O",
        }
    }
}

/// A single track. Clips are kept sorted by start and never overlap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    /// Display name, e.g. `V1`, `A2`.
    pub name: String,
    pub kind: TrackKind,
    pub muted: bool,
    pub solo: bool,
    pub locked: bool,
    clips: Vec<Clip>,
}

impl Track {
    #[must_use]
    pub fn new(id: TrackId, name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            muted: false,
            solo: false,
            locked: false,
            clips: Vec::new(),
        }
    }

    #[must_use]
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }

    /// End of the last clip, i.e. the track's used length.
    #[must_use]
    pub fn duration(&self) -> Frame {
        self.clips.last().map_or(Frame::ZERO, Clip::end)
    }

    #[must_use]
    pub fn index_at(&self, frame: Frame) -> Option<usize> {
        self.clips.iter().position(|c| c.contains(frame))
    }

    #[must_use]
    pub fn clip_at(&self, frame: Frame) -> Option<&Clip> {
        self.index_at(frame).map(|i| &self.clips[i])
    }

    #[must_use]
    pub fn index_of(&self, id: ClipId) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    #[must_use]
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.index_of(id).map(|i| &self.clips[i])
    }

    /// Mutable clip access is crate-internal: all outside mutation goes
    /// through a timeline primitive so invariants are re-checked.
    pub(crate) fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.index_of(id).map(|i| &mut self.clips[i])
    }

    pub(crate) fn clips_mut(&mut self) -> &mut Vec<Clip> {
        &mut self.clips
    }

    // -- transitions (spec §6.2) -----------------------------------------

    /// Whether the clip at `index` starts on a cut, i.e. the previous clip
    /// ends exactly where it begins. A transition needs two clips to join.
    #[must_use]
    pub fn abuts_previous(&self, index: usize) -> bool {
        index > 0
            && self
                .clips
                .get(index - 1)
                .zip(self.clips.get(index))
                .is_some_and(|(prev, c)| prev.end() == c.start)
    }

    /// Whether a transition of this length can be built on the cut at
    /// `index`, and why not when it cannot.
    ///
    /// The overlap is made of handle frames, so this is where spec §6.2's
    /// "fails with a clear error rather than silently shortening" lives: the
    /// answer is computed before anything is written.
    pub fn check_transition(&self, index: usize, t: &Transition) -> Result<(), CoreError> {
        let reject = |reason: String| Err(CoreError::CannotTransition { reason });
        if t.kind.trim().is_empty() {
            return reject("a transition needs a type".into());
        }
        if t.duration == Frame::ZERO {
            return reject("a transition cannot be zero frames long".into());
        }
        if !self.abuts_previous(index) {
            return reject("there is no cut here: a transition joins two abutting clips".into());
        }
        let (Some(prev), Some(clip)) = (self.clips.get(index - 1), self.clips.get(index)) else {
            return reject("there is no cut here: a transition joins two abutting clips".into());
        };
        let (head, tail) = (t.head(), t.tail());
        // Room already taken by the transitions on the *other* cuts of these
        // two clips. Without this an overlap could swallow a neighbouring
        // one and the projection would emit a negative-length entry.
        let taken_before = prev.transition_in.as_ref().map_or(0, Transition::tail);
        let taken_after = self
            .clips
            .get(index + 1)
            .filter(|next| next.start == clip.end())
            .and_then(|next| next.transition_in.as_ref())
            .map_or(0, Transition::head);
        if head + taken_before > prev.duration.get() || tail + taken_after > clip.duration.get() {
            return reject(format!(
                "a {}-frame transition would run into the next one",
                t.duration.get()
            ));
        }
        // The overlap has to fit inside both clips as well as inside their
        // handles, or it would reach across a neighbouring cut.
        if head > prev.duration.get() || tail > clip.duration.get() {
            return reject(format!(
                "a {}-frame transition is longer than the clips it joins",
                t.duration.get()
            ));
        }
        let short = |have: u64, need: u64| need.saturating_sub(have);
        let missing = short(prev.tail_handle().unwrap_or(u64::MAX), tail)
            .max(short(clip.head_handle().unwrap_or(u64::MAX), head));
        if missing > 0 {
            return reject(format!(
                "not enough handle frames for a {}-frame transition (short by {missing})",
                t.duration.get()
            ));
        }
        Ok(())
    }

    /// Forget transitions whose cut no longer exists or no longer fits.
    ///
    /// Every edit primitive ends here (via `Timeline::settle`): a ripple that
    /// removes a neighbour must resolve the transition rather than orphan it
    /// on a cut that is gone (plan.md Phase 9f).
    pub(crate) fn prune_transitions(&mut self) {
        let stale: Vec<usize> = (0..self.clips.len())
            .filter(|&i| {
                self.clips[i]
                    .transition_in
                    .as_ref()
                    .is_some_and(|t| self.check_transition(i, t).is_err())
            })
            .collect();
        for i in stale {
            self.clips[i].transition_in = None;
        }
    }

    /// The clip range plus any transition attached to either of its cuts -
    /// the `ac` text object (spec §4.1).
    #[must_use]
    pub fn transition_range(&self, id: ClipId) -> Option<(Frame, Frame)> {
        let i = self.index_of(id)?;
        let clip = self.clips.get(i)?;
        let start = clip
            .transition_in
            .as_ref()
            .map_or(clip.start, |t| t.span(clip.start).0);
        let end = self
            .clips
            .get(i + 1)
            .filter(|next| next.start == clip.end())
            .and_then(|next| next.transition_in.as_ref())
            .map_or(clip.end(), |t| t.span(clip.end()).1);
        Some((start, end))
    }

    /// Index of the incoming clip of the cut nearest `frame`, if the track
    /// has one. Ties go to the cut at or after the playhead, which is where
    /// `gx` puts a transition when the playhead sits exactly on a cut.
    #[must_use]
    pub fn nearest_cut(&self, frame: Frame) -> Option<usize> {
        (1..self.clips.len())
            .filter(|&i| self.abuts_previous(i))
            .min_by_key(|&i| {
                let cut = self.clips[i].start.get();
                (cut.abs_diff(frame.get()), cut < frame.get())
            })
    }

    /// Index of the incoming clip whose transition covers `frame`.
    #[must_use]
    pub fn transition_at(&self, frame: Frame) -> Option<usize> {
        (0..self.clips.len()).find(|&i| {
            self.clips[i]
                .transition_in
                .as_ref()
                .is_some_and(|t| t.covers(self.clips[i].start, frame))
        })
    }

    /// Whether `[start, end)` is free of clips, ignoring `ignore`.
    #[must_use]
    pub fn range_is_free(&self, start: Frame, end: Frame, ignore: Option<ClipId>) -> bool {
        !self
            .clips
            .iter()
            .filter(|c| Some(c.id) != ignore)
            .any(|c| c.start < end && start < c.end())
    }

    /// Insert keeping start order. Caller must have checked for overlap.
    pub(crate) fn insert_sorted(&mut self, clip: Clip) {
        let at = self
            .clips
            .iter()
            .position(|c| c.start > clip.start)
            .unwrap_or(self.clips.len());
        self.clips.insert(at, clip);
    }

    /// Structural invariants for one track (plan.md Phase 1).
    pub(crate) fn check_invariants(&self) -> Result<(), CoreError> {
        let mut prev_end = Frame::ZERO;
        for (i, c) in self.clips.iter().enumerate() {
            if c.duration == Frame::ZERO {
                return Err(CoreError::InvariantViolation(format!(
                    "clip {} on {} has zero duration",
                    c.id, self.name
                )));
            }
            if i > 0 && c.start < prev_end {
                return Err(CoreError::InvariantViolation(format!(
                    "clip {} on {} overlaps the previous clip",
                    c.id, self.name
                )));
            }
            if let Some(m) = &c.media
                && c.source_out() > m.length
            {
                return Err(CoreError::InvariantViolation(format!(
                    "clip {} on {} runs past the end of {}",
                    c.id, self.name, m.path
                )));
            }
            prev_end = c.end();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ClipId;

    fn track_with(spans: &[(u64, u64)]) -> Track {
        let mut t = Track::new(TrackId(1), "V1", TrackKind::Video);
        for (i, (s, d)) in spans.iter().enumerate() {
            t.insert_sorted(Clip::generated(
                ClipId(i as u64 + 1),
                format!("c{i}"),
                Frame(*s),
                Frame(*d),
            ));
        }
        t
    }

    #[test]
    fn lookup_by_frame_respects_half_open_bounds() {
        let t = track_with(&[(0, 100), (100, 150)]);
        assert_eq!(t.clip_at(Frame(0)).map(|c| c.id), Some(ClipId(1)));
        assert_eq!(t.clip_at(Frame(100)).map(|c| c.id), Some(ClipId(2)));
        assert_eq!(t.clip_at(Frame(250)), None);
        assert_eq!(t.duration(), Frame(250));
    }

    #[test]
    fn insertion_keeps_start_order() {
        let mut t = track_with(&[(200, 50)]);
        t.insert_sorted(Clip::generated(ClipId(9), "x", Frame(0), Frame(10)));
        let starts: Vec<u64> = t.clips().iter().map(|c| c.start.get()).collect();
        assert_eq!(starts, vec![0, 200]);
        assert!(t.check_invariants().is_ok());
    }

    #[test]
    fn range_freedom_ignores_the_named_clip() {
        let t = track_with(&[(0, 100)]);
        assert!(!t.range_is_free(Frame(50), Frame(60), None));
        assert!(t.range_is_free(Frame(50), Frame(60), Some(ClipId(1))));
        assert!(t.range_is_free(Frame(100), Frame(200), None));
    }

    #[test]
    fn overlap_is_an_invariant_violation() {
        let mut t = track_with(&[(0, 100)]);
        t.clips_mut()
            .push(Clip::generated(ClipId(7), "b", Frame(50), Frame(10)));
        assert!(t.check_invariants().is_err());
    }
}
