//! The timeline: tracks, playhead, markers, marks, registers, invariants.
//!
//! Mutation lives in [`crate::edit`] and [`crate::trim`]. Everything here is
//! construction, query, and verification.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::clip::Clip;
use crate::error::CoreError;
use crate::id::{ClipId, GroupId, IdGen, TrackId};
use crate::time::{Frame, TimelineProps};
use crate::track::{Track, TrackKind};
use crate::transition::Transition;

/// A named point on the timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub frame: Frame,
    pub label: String,
}

/// A vim-style mark set with `m<char>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mark {
    pub frame: Frame,
    pub track: Option<TrackId>,
}

/// Yanked content. Clip starts are normalised so the first clip is at zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Register {
    pub clips: Vec<Clip>,
    /// Span of the yanked range, including any leading or trailing gap.
    pub span: Frame,
}

impl Register {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }
}

/// Playhead position: a frame plus the focused track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playhead {
    pub frame: Frame,
    pub track: TrackId,
}

/// The edit buffer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeline {
    pub props: TimelineProps,
    pub markers: Vec<Marker>,
    pub marks: BTreeMap<char, Mark>,
    pub registers: BTreeMap<char, Register>,
    playhead: Playhead,
    tracks: Vec<Track>,
    ids: IdGen,
}

impl Timeline {
    /// A timeline with one video and one audio track, focused on `V1`.
    #[must_use]
    pub fn new(props: TimelineProps) -> Self {
        let mut ids = IdGen::new();
        let v1 = Track::new(ids.track(), "V1", TrackKind::Video);
        let a1 = Track::new(ids.track(), "A1", TrackKind::Audio);
        let focus = v1.id;
        Self {
            props,
            markers: Vec::new(),
            marks: BTreeMap::new(),
            registers: BTreeMap::new(),
            playhead: Playhead {
                frame: Frame::ZERO,
                track: focus,
            },
            tracks: vec![v1, a1],
            ids,
        }
    }

    // -- tracks ----------------------------------------------------------

    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub fn add_track(&mut self, kind: TrackKind) -> TrackId {
        let id = self.ids.track();
        let name = self.next_track_name(kind);
        self.tracks.push(Track::new(id, name, kind));
        id
    }

    /// The name [`Timeline::add_track`] would give the next track of `kind`.
    ///
    /// The lowest free index, not the count: after removing `V1` from a
    /// `V1`/`V2` stack, counting would propose `V2` again and mint a
    /// duplicate name - a state [`Timeline::add_track_with_id`] rejects and
    /// [`Timeline::track_by_name`] cannot resolve.
    #[must_use]
    pub fn next_track_name(&self, kind: TrackKind) -> String {
        let prefix = kind.prefix();
        // One more candidate than tracks that exist, so a free one always
        // exists and the search terminates.
        (1..=self.tracks.len() + 1)
            .map(|n| format!("{prefix}{n}"))
            .find(|name| self.track_by_name(name).is_none())
            .unwrap_or_else(|| format!("{prefix}1"))
    }

    /// Add a track with an id and name chosen by the caller.
    ///
    /// The command layer needs this so that redoing an import reproduces the
    /// same track ids, exactly as `split_at_with_id` does for clips
    ///.
    pub fn add_track_with_id(
        &mut self,
        id: TrackId,
        name: impl Into<String>,
        kind: TrackKind,
    ) -> Result<(), CoreError> {
        let name = name.into();
        if self.track(id).is_some() {
            return Err(CoreError::DuplicateTrack(id.to_string()));
        }
        if self.track_by_name(&name).is_some() {
            return Err(CoreError::DuplicateTrack(name));
        }
        self.tracks.push(Track::new(id, name, kind));
        self.settle();
        Ok(())
    }

    /// Reserve `n` raw ids for a caller that must pin them before it can
    /// build its commands - an import names the track a clip goes on before
    /// the `AddTrack` that creates it has run.
    ///
    /// Ids are monotonic and never reused, so ids reserved by a plan that is
    /// then rejected are simply skipped.
    pub fn reserve_ids(&mut self, n: usize) -> Vec<u64> {
        (0..n).map(|_| self.ids.clip().get()).collect()
    }

    /// Allocate the next track id, for a command that must record the id it
    /// used (see [`Timeline::add_track_with_id`]).
    pub fn new_track_id(&mut self) -> TrackId {
        self.ids.track()
    }

    /// Remove an empty track. A track with clips is refused: dropping content
    /// is an edit, and has to be one the log can see.
    pub fn remove_track(&mut self, id: TrackId) -> Result<(String, TrackKind), CoreError> {
        let t = self.require_track(id)?;
        if !t.is_empty() {
            return Err(CoreError::TrackNotEmpty(t.name.clone()));
        }
        if self.tracks.len() == 1 {
            return Err(CoreError::TrackNotEmpty(t.name.clone()));
        }
        let (name, kind) = (t.name.clone(), t.kind);
        self.tracks.retain(|t| t.id != id);
        if self.playhead.track == id {
            // The playhead cannot focus a track that is gone.
            self.playhead.track = self.tracks[0].id;
        }
        self.settle();
        Ok((name, kind))
    }

    /// Move a track to `to` in the stack, returning the index it came from.
    ///
    /// Order is stacking order, not content: no clip changes track or frame,
    /// so the move is always legal on a non-empty track. `to` is the index
    /// the track ends up at once it has been taken out of the stack, which
    /// makes the inverse a move back to the old index.
    pub fn move_track(&mut self, id: TrackId, to: usize) -> Result<usize, CoreError> {
        let from = self
            .tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| CoreError::NoSuchTrack(id.to_string()))?;
        if to >= self.tracks.len() {
            return Err(CoreError::TrackIndexOutOfRange {
                index: to,
                count: self.tracks.len(),
            });
        }
        let track = self.tracks.remove(from);
        self.tracks.insert(to, track);
        self.settle();
        Ok(from)
    }

    #[must_use]
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    #[must_use]
    pub fn track_by_name(&self, name: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.name == name)
    }

    pub(crate) fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Look up a track or produce the user-facing "no such track" error.
    pub(crate) fn require_track(&self, id: TrackId) -> Result<&Track, CoreError> {
        self.track(id)
            .ok_or_else(|| CoreError::NoSuchTrack(id.to_string()))
    }

    pub(crate) fn require_track_mut(&mut self, id: TrackId) -> Result<&mut Track, CoreError> {
        let name = id.to_string();
        self.track_mut(id).ok_or(CoreError::NoSuchTrack(name))
    }

    // -- clips -----------------------------------------------------------

    /// Allocate the next clip id. Used when building clips to insert.
    pub fn new_clip_id(&mut self) -> ClipId {
        self.ids.clip()
    }

    /// The id the next allocation would use.
    ///
    /// The id generator is part of the timeline's serialized state, so undo
    /// and rollback have to be able to put it back where it was - see
    /// [`Timeline::set_id_cursor`].
    #[must_use]
    pub fn id_cursor(&self) -> u64 {
        self.ids.peek()
    }

    /// Move the id cursor, never below an id that is already in use.
    ///
    /// Used by the command layer: a rejected command hands back the ids it
    /// reserved, and undo/redo restore the cursor the recorded state had, so
    /// history navigation is byte-exact.
    pub fn set_id_cursor(&mut self, next: u64) {
        let mut floor = 1;
        for t in &self.tracks {
            floor = floor.max(t.id.get() + 1);
            for c in t.clips() {
                floor = floor.max(c.id.get() + 1);
                if let Some(g) = c.group {
                    floor = floor.max(g.get() + 1);
                }
            }
        }
        self.ids.set(next.max(floor));
    }

    /// Allocate the next group id, for a command that links clips with
    /// [`Timeline::set_group`] and must record the id it used.
    pub fn new_group_id(&mut self) -> GroupId {
        self.ids.group()
    }

    /// Find a clip anywhere on the timeline.
    #[must_use]
    pub fn find_clip(&self, id: ClipId) -> Option<(TrackId, &Clip)> {
        self.tracks
            .iter()
            .find_map(|t| t.clip(id).map(|c| (t.id, c)))
    }

    /// Total timeline duration: the end of the last clip on any track.
    #[must_use]
    pub fn duration(&self) -> Frame {
        self.tracks
            .iter()
            .map(Track::duration)
            .max()
            .unwrap_or(Frame::ZERO)
    }

    // -- playhead --------------------------------------------------------

    #[must_use]
    pub fn playhead(&self) -> Playhead {
        self.playhead
    }

    pub fn set_playhead_frame(&mut self, frame: Frame) {
        self.playhead.frame = frame;
    }

    pub fn focus_track(&mut self, id: TrackId) -> Result<(), CoreError> {
        self.require_track(id)?;
        self.playhead.track = id;
        Ok(())
    }

    // -- grouping ----------------------------------------------

    /// Link clips into one group. Linkage is per-clip, not per-track.
    ///
    /// Validate-then-mutate: an unknown clip id, a duplicate, or a set of
    /// clips that are not frame-aligned is rejected before anything changes.
    pub fn link(&mut self, clips: &[ClipId]) -> Result<GroupId, CoreError> {
        if clips.len() < 2 {
            return Err(CoreError::CannotLink {
                reason: "a link group needs at least two clips".into(),
            });
        }
        let mut extents = Vec::with_capacity(clips.len());
        for id in clips {
            let (_, c) = self
                .find_clip(*id)
                .ok_or_else(|| CoreError::NoSuchClip(id.to_string()))?;
            extents.push((c.start, c.end()));
        }
        if extents.windows(2).any(|w| w[0] != w[1]) {
            return Err(CoreError::CannotLink {
                reason: "linked clips must start and end on the same frames".into(),
            });
        }
        let group = self.ids.group();
        for id in clips {
            for t in &mut self.tracks {
                if let Some(c) = t.clip_mut(*id) {
                    c.group = Some(group);
                }
            }
        }
        Ok(group)
    }

    /// Remove one clip from its group.
    pub fn unlink(&mut self, clip: ClipId) -> Result<(), CoreError> {
        let (track, _) = self
            .find_clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
        {
            c.group = None;
        }
        Ok(())
    }

    /// Put one clip in a group, or take it out of one.
    ///
    /// The command layer needs this to invert `link`/`unlink` exactly: a clip
    /// may have belonged to a different group before.
    pub fn set_group(&mut self, clip: ClipId, group: Option<GroupId>) -> Result<(), CoreError> {
        let (track, c) = self
            .find_clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        let extent = (c.start, c.end());
        if let Some(g) = group
            && let Some(other) = self
                .tracks
                .iter()
                .flat_map(Track::clips)
                .find(|o| o.group == Some(g) && o.id != clip)
            && (other.start, other.end()) != extent
        {
            return Err(CoreError::CannotLink {
                reason: "linked clips must start and end on the same frames".into(),
            });
        }
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
        {
            c.group = group;
        }
        self.settle();
        Ok(())
    }

    /// Replace a clip's non-destructive properties.
    ///
    /// Validate-then-mutate: nonsense fades or transforms are rejected before
    /// anything is written.
    pub fn set_clip_props(
        &mut self,
        track: TrackId,
        clip: ClipId,
        props: crate::clip::ClipProps,
    ) -> Result<(), CoreError> {
        let t = self.require_track(track)?;
        let c = t
            .clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        let reject = |reason: &str| CoreError::InvalidProps {
            reason: reason.to_string(),
        };
        if props.fade_in.get() + props.fade_out.get() > c.duration.get() {
            return Err(reject("the fades are longer than the clip"));
        }
        if !props.gain_db.is_finite() {
            return Err(reject("gain must be a finite number of decibels"));
        }
        let tf = props.transform;
        if !(tf.x.is_finite() && tf.y.is_finite() && tf.scale.is_finite()) {
            return Err(reject("transform values must be finite"));
        }
        if tf.scale <= 0.0 {
            return Err(reject("scale must be greater than zero"));
        }
        if !(0.0..=1.0).contains(&tf.opacity) {
            return Err(reject("opacity must be between 0 and 1"));
        }
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
        {
            c.props = props;
        }
        self.settle();
        Ok(())
    }

    /// Set a subtitle clip's text, returning what it said before.
    ///
    /// Only a clip that already carries text: a media clip has no text
    /// payload, and inventing one would put a subtitle on a video.
    pub fn set_clip_text(
        &mut self,
        track: TrackId,
        clip: ClipId,
        text: impl Into<String>,
    ) -> Result<String, CoreError> {
        let t = self.require_track(track)?;
        let c = t
            .clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        if c.text.is_none() {
            return Err(CoreError::InvalidProps {
                reason: "that clip carries no text to edit".to_string(),
            });
        }
        let text = text.into();
        let mut previous = String::new();
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
        {
            previous = c.text.replace(text).unwrap_or_default();
        }
        self.settle();
        Ok(previous)
    }

    /// Set a track's mute flag.
    ///
    /// Track state, not clip state: muting changes what the backend renders
    /// and nothing about the media or the clips.
    pub fn set_track_muted(&mut self, track: TrackId, muted: bool) -> Result<(), CoreError> {
        self.require_track(track)?;
        if let Some(t) = self.track_mut(track) {
            t.muted = muted;
        }
        Ok(())
    }

    /// Set a track's solo flag.
    ///
    /// Solo is exclusive by *effect*, not by state: any soloed track silences
    /// every non-soloed one, which the backend resolves at projection time.
    /// Several tracks may therefore be soloed at once.
    pub fn set_track_solo(&mut self, track: TrackId, solo: bool) -> Result<(), CoreError> {
        self.require_track(track)?;
        if let Some(t) = self.track_mut(track) {
            t.solo = solo;
        }
        Ok(())
    }

    // -- transitions -----------------------------------------

    /// Attach a transition to the cut at `clip`'s start, or remove one.
    ///
    /// Returns what was there before, so the command layer can invert
    /// exactly. Validate-then-mutate: a cut without the handle frames to
    /// build the overlap is refused and the timeline is untouched.
    pub fn set_transition(
        &mut self,
        track: TrackId,
        clip: ClipId,
        transition: Option<Transition>,
    ) -> Result<Option<Transition>, CoreError> {
        let t = self.require_track(track)?;
        let index = t
            .index_of(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        if let Some(new) = &transition {
            t.check_transition(index, new)?;
        }
        let mut previous = None;
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
        {
            previous = std::mem::replace(&mut c.transition_in, transition);
        }
        self.settle();
        Ok(previous)
    }

    /// The cut nearest `frame` on `track`, as `(incoming clip, cut frame)`.
    ///
    /// This is what `gx` and `dax` act on.
    #[must_use]
    pub fn nearest_cut(&self, track: TrackId, frame: Frame) -> Option<(ClipId, Frame)> {
        let t = self.track(track)?;
        let i = t.nearest_cut(frame)?;
        t.clips().get(i).map(|c| (c.id, c.start))
    }

    /// The transition under `frame`, else the one on the nearest cut.
    #[must_use]
    pub fn transition_at(&self, track: TrackId, frame: Frame) -> Option<(ClipId, &Transition)> {
        let t = self.track(track)?;
        let i = t.transition_at(frame).or_else(|| {
            t.nearest_cut(frame)
                .filter(|&i| t.clips().get(i).is_some_and(|c| c.transition_in.is_some()))
        })?;
        let c = t.clips().get(i)?;
        c.transition_in.as_ref().map(|tr| (c.id, tr))
    }

    /// Flag a clip's media offline, or bring it back (Phase 0 policy).
    ///
    /// The project stays editable either way; the flag is what makes the
    /// backend render a placeholder and what blocks export.
    pub fn set_media_offline(&mut self, clip: ClipId, offline: bool) -> Result<(), CoreError> {
        let (track, c) = self
            .find_clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        if c.media.is_none() {
            return Err(CoreError::InvalidProps {
                reason: "a generated clip has no media to take offline".into(),
            });
        }
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
            && let Some(m) = c.media.as_mut()
        {
            m.offline = offline;
        }
        Ok(())
    }

    /// Point a clip's media at a different file (`:relink`).
    ///
    /// The offline flag is set explicitly by the caller rather than inferred
    /// here: `davimci-core` does no I/O, so it cannot know whether the new
    /// path exists. Returns the previous `(path, offline)` so the command
    /// layer can invert exactly.
    pub fn set_media_source(
        &mut self,
        clip: ClipId,
        path: impl Into<String>,
        offline: bool,
    ) -> Result<(String, bool), CoreError> {
        let path = path.into();
        if path.is_empty() {
            return Err(CoreError::InvalidProps {
                reason: "a media path cannot be empty".into(),
            });
        }
        let (track, c) = self
            .find_clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        if c.media.is_none() {
            return Err(CoreError::InvalidProps {
                reason: "a generated clip has no media to relink".into(),
            });
        }
        let mut previous = (String::new(), false);
        if let Some(t) = self.track_mut(track)
            && let Some(c) = t.clip_mut(clip)
            && let Some(m) = c.media.as_mut()
        {
            previous = (std::mem::replace(&mut m.path, path), m.offline);
            m.offline = offline;
        }
        self.settle();
        Ok(previous)
    }

    /// Every clip in `group`, as `(track, clip id)` pairs.
    #[must_use]
    pub fn group_members(&self, group: GroupId) -> Vec<(TrackId, ClipId)> {
        self.tracks
            .iter()
            .flat_map(|t| {
                t.clips()
                    .iter()
                    .filter(move |c| c.group == Some(group))
                    .map(move |c| (t.id, c.id))
            })
            .collect()
    }

    // -- invariants ------------------------------------------------------

    /// Full structural check.
    pub fn check_invariants(&self) -> Result<(), CoreError> {
        for t in &self.tracks {
            t.check_invariants()?;
        }
        // A group is one object seen on several tracks, so it holds at most
        // one clip per track. Alignment is not an invariant: an edit that
        // moves a group reaches its members one at a time, and the states in
        // between are legal even though only the last one is aligned.
        let mut members: BTreeSet<(GroupId, TrackId)> = BTreeSet::new();
        for t in &self.tracks {
            for c in t.clips() {
                if let Some(g) = c.group
                    && !members.insert((g, t.id))
                {
                    return Err(CoreError::InvariantViolation(format!(
                        "group {g} has two clips on track {}",
                        t.name
                    )));
                }
            }
        }
        if self.track(self.playhead.track).is_none() {
            return Err(CoreError::InvariantViolation(
                "playhead focuses a track that does not exist".into(),
            ));
        }
        // Identity is unique: two tracks with one name or one id, or two
        // clips with one id, make every lookup in the model ambiguous.
        let mut track_ids = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut clip_ids = BTreeSet::new();
        for t in &self.tracks {
            if !track_ids.insert(t.id) {
                return Err(CoreError::InvariantViolation(format!(
                    "track id {} is used twice",
                    t.id
                )));
            }
            if !names.insert(t.name.as_str()) {
                return Err(CoreError::InvariantViolation(format!(
                    "track name {} is used twice",
                    t.name
                )));
            }
            for c in t.clips() {
                if !clip_ids.insert(c.id) {
                    return Err(CoreError::InvariantViolation(format!(
                        "clip id {} is used twice",
                        c.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Panicking form, for use at the end of every mutation in tests and
    /// debug builds. The only sanctioned panic path.
    #[allow(clippy::panic)]
    pub fn assert_invariants(&self) {
        if let Err(e) = self.check_invariants() {
            crate::assert_invariant!(false, "{e}");
        }
    }

    /// Drop transitions a mutation has invalidated, then check invariants.
    ///
    /// Every mutating primitive ends here. A transition lives on a cut, and
    /// an edit can take that cut away: the
    /// transition goes with it rather than being left pointing at nothing.
    pub(crate) fn settle(&mut self) {
        for t in &mut self.tracks {
            t.prune_transitions();
        }
        self.debug_assert_invariants();
    }

    /// Debug-only invariant check, called after every mutating primitive.
    pub(crate) fn debug_assert_invariants(&self) {
        #[cfg(debug_assertions)]
        self.assert_invariants();
    }

    // -- dump ------------------------------------------------------------

    /// Compact textual dump, e.g. `V1: [a 0-100][b 100-250]`.
    ///
    /// This is the snapshot format ripple/lift diffs are reviewed in, so it
    /// must stay stable and one line per track.
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for t in &self.tracks {
            let _ = write!(out, "{}:", t.name);
            if t.muted {
                out.push_str(" (muted)");
            }
            let mut cursor = Frame::ZERO;
            for c in t.clips() {
                if c.start > cursor {
                    let _ = write!(out, "<gap {}>", c.start.get() - cursor.get());
                }
                let _ = write!(out, "[{} {}-{}", c.label, c.start.get(), c.end().get());
                if let Some(g) = c.group {
                    let _ = write!(out, " {g}");
                }
                out.push(']');
                cursor = c.end();
            }
            if t.is_empty() {
                out.push_str(" -");
            }
            out.push('\n');
        }
        out
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new(TimelineProps::default())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testing::fixture;

    #[test]
    fn a_new_timeline_has_v1_and_a1_and_is_valid() {
        let tl = Timeline::new(TimelineProps::default());
        assert_eq!(tl.tracks().len(), 2);
        assert!(tl.track_by_name("V1").is_some());
        assert!(tl.track_by_name("A1").is_some());
        assert_eq!(tl.playhead().frame, Frame::ZERO);
        assert_eq!(tl.duration(), Frame::ZERO);
        tl.assert_invariants();
    }

    #[test]
    fn mute_and_solo_are_track_state_and_reject_unknown_tracks() {
        let mut tl = fixture(&[("A1", &[(0, 10, "a")]), ("A2", &[(0, 10, "b")])]);
        let a1 = tl.tracks()[0].id;
        tl.set_track_muted(a1, true).unwrap();
        tl.set_track_solo(a1, true).unwrap();
        assert!(tl.tracks()[0].muted && tl.tracks()[0].solo);
        assert!(!tl.tracks()[1].muted && !tl.tracks()[1].solo);
        tl.set_track_muted(a1, false).unwrap();
        assert!(!tl.tracks()[0].muted);
        assert!(tl.set_track_muted(TrackId(9999), true).is_err());
        tl.assert_invariants();
    }

    #[test]
    fn offline_flagging_needs_media_and_leaves_the_clip_editable() {
        let mut tl = crate::testing::media_fixture(&[(0, 10, 0, 100)]);
        let media_clip = tl.tracks()[0].clips()[0].id;
        tl.set_media_offline(media_clip, true).unwrap();
        let (_, c) = tl.find_clip(media_clip).unwrap();
        assert!(c.media.as_ref().is_some_and(|m| m.offline));
        assert_eq!(c.duration, Frame(10), "going offline is not an edit");
        tl.set_media_offline(media_clip, false).unwrap();
        assert!(
            tl.find_clip(media_clip)
                .is_some_and(|(_, c)| c.media.as_ref().is_some_and(|m| !m.offline))
        );

        let mut generated = fixture(&[("V1", &[(0, 10, "a")])]);
        let gen_clip = generated.tracks()[0].clips()[0].id;
        assert!(generated.set_media_offline(gen_clip, true).is_err());
    }

    #[test]
    fn relinking_swaps_the_path_and_reports_the_previous_one() {
        let mut tl = crate::testing::media_fixture(&[(0, 10, 0, 100)]);
        let clip = tl.tracks()[0].clips()[0].id;
        tl.set_media_offline(clip, true).unwrap();
        let (old_path, was_offline) = tl.set_media_source(clip, "/new.mkv", false).unwrap();
        assert!(was_offline);
        assert_ne!(old_path, "/new.mkv");
        let (_, c) = tl.find_clip(clip).unwrap();
        let media = c.media.as_ref().unwrap();
        assert_eq!(media.path, "/new.mkv");
        assert!(!media.offline, "a relinked clip comes back online");

        assert!(tl.set_media_source(clip, "", false).is_err());
        assert!(tl.set_media_source(ClipId(9999), "/x.mkv", false).is_err());
    }

    #[test]
    fn added_tracks_are_numbered_per_kind() {
        let mut tl = Timeline::new(TimelineProps::default());
        tl.add_track(TrackKind::Audio);
        tl.add_track(TrackKind::Text);
        assert!(tl.track_by_name("A2").is_some());
        assert!(tl.track_by_name("T1").is_some());
    }

    /// Regression: naming counted tracks of the kind instead of taking the
    /// lowest free index, so removing `V1` from a `V1`/`V2` stack made the
    /// next added video track a second `V2`.
    #[test]
    fn a_removed_track_does_not_leave_the_next_name_colliding() {
        let mut tl = Timeline::new(TimelineProps::default());
        tl.add_track(TrackKind::Video); // V2
        let (v1, a1) = (
            tl.track_by_name("V1").map(|t| t.id),
            tl.track_by_name("A1").map(|t| t.id),
        );
        let (Some(v1), Some(a1)) = (v1, a1) else {
            unreachable!()
        };
        tl.focus_track(a1).unwrap();
        tl.remove_track(v1).unwrap();

        assert_eq!(tl.next_track_name(TrackKind::Video), "V1");
        tl.add_track(TrackKind::Video);
        let names: Vec<&str> = tl.tracks().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["A1", "V2", "V1"]);
        tl.assert_invariants();
    }

    #[test]
    fn dump_shows_clips_and_gaps() {
        let tl = fixture(&[("V1", &[(0, 100, "a"), (150, 50, "b")])]);
        assert_eq!(tl.dump(), "V1:[a 0-100]<gap 50>[b 150-200]\nA1: -\n");
    }

    #[test]
    fn linking_requires_frame_alignment() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 90, "a-aud")])]);
        let v = tl.track_by_name("V1").map(|t| t.clips()[0].id);
        let a = tl.track_by_name("A1").map(|t| t.clips()[0].id);
        let (Some(v), Some(a)) = (v, a) else {
            unreachable!()
        };
        assert!(tl.link(&[v, a]).is_err());
        assert!(tl.find_clip(v).is_some_and(|(_, c)| c.group.is_none()));
    }

    #[test]
    fn linked_clips_share_a_group_and_can_be_unlinked() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "a-aud")])]);
        let v = tl
            .track_by_name("V1")
            .map_or(crate::id::ClipId(0), |t| t.clips()[0].id);
        let a = tl
            .track_by_name("A1")
            .map_or(crate::id::ClipId(0), |t| t.clips()[0].id);
        let g = tl.link(&[v, a]).unwrap_or(GroupId(0));
        assert_eq!(tl.group_members(g).len(), 2);
        tl.assert_invariants();
        assert!(tl.unlink(a).is_ok());
        assert_eq!(tl.group_members(g).len(), 1);
    }

    #[test]
    fn link_rejects_a_single_clip() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v = tl.track_by_name("V1").map(|t| t.clips()[0].id);
        assert!(tl.link(&[v.unwrap_or(ClipId(0))]).is_err());
    }

    #[test]
    fn the_id_cursor_rewinds_but_never_aliases_a_live_clip() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let before = tl.id_cursor();
        let a = tl.new_clip_id();
        tl.set_id_cursor(before);
        assert_eq!(tl.id_cursor(), before);
        assert_eq!(tl.new_clip_id(), a);
        // Rewinding onto a live clip's id is clamped away.
        tl.set_id_cursor(1);
        let next = tl.new_clip_id();
        assert!(tl.find_clip(next).is_none());
        assert!(next.get() >= a.get());
    }

    #[test]
    fn set_group_rejects_a_misaligned_member() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 90, "a-aud")])]);
        let v = tl.track_by_name("V1").map(|t| t.clips()[0].id).unwrap();
        let a = tl.track_by_name("A1").map(|t| t.clips()[0].id).unwrap();
        let g = GroupId(77);
        assert!(tl.set_group(v, Some(g)).is_ok());
        assert!(tl.set_group(a, Some(g)).is_err());
        assert!(tl.set_group(v, None).is_ok());
    }

    #[test]
    fn clip_props_are_validated_before_they_are_written() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let track = tl.track_by_name("V1").map(|t| t.id).unwrap();
        let clip = tl.track_by_name("V1").map(|t| t.clips()[0].id).unwrap();
        let before = tl.clone();

        let mut props = crate::clip::ClipProps {
            fade_in: Frame(80),
            fade_out: Frame(80),
            ..Default::default()
        };
        assert!(matches!(
            tl.set_clip_props(track, clip, props),
            Err(CoreError::InvalidProps { .. })
        ));
        props.fade_out = Frame(20);
        assert!(tl.set_clip_props(track, clip, props).is_ok());
        assert_eq!(
            tl.find_clip(clip).map(|(_, c)| c.props.fade_in),
            Some(Frame(80))
        );

        let mut bad = crate::clip::ClipProps::default();
        bad.transform.opacity = 2.0_f32;
        assert!(tl.set_clip_props(track, clip, bad).is_err());
        bad.transform.opacity = 1.0;
        bad.transform.scale = 0.0;
        assert!(tl.set_clip_props(track, clip, bad).is_err());
        assert_ne!(tl, before);
    }

    /// The overlap is built from handle frames, so a cut between
    /// two clips that have none is refused outright rather than shortened.
    #[test]
    fn a_transition_needs_handles_on_both_sides() {
        // Two abutting clips, each with 20 frames of head and tail handle.
        let mut tl = crate::testing::media_fixture(&[(0, 100, 20, 140), (100, 100, 20, 140)]);
        let track = tl.tracks()[0].id;
        let right = tl.tracks()[0].clips()[1].id;
        let before = tl.clone();

        assert!(
            tl.set_transition(track, right, Some(Transition::new("dissolve", Frame(80))))
                .is_err(),
            "40 frames of handle each way cannot make an 80-frame overlap"
        );
        assert_eq!(tl, before, "a refused transition changes nothing");

        assert_eq!(
            tl.set_transition(track, right, Some(Transition::of("dissolve"))),
            Ok(None)
        );
        let (_, c) = tl.find_clip(right).unwrap();
        assert_eq!(
            c.transition_in.as_ref().map(|t| t.duration),
            Some(Frame(12))
        );
        assert_eq!(c.duration, Frame(100), "a transition moves no clip");
        tl.assert_invariants();

        // Removing reports what was there, so the command layer can invert.
        let previous = tl.set_transition(track, right, None).unwrap();
        assert_eq!(previous.map(|t| t.kind), Some("dissolve".to_string()));
    }

    #[test]
    fn a_transition_needs_a_cut_not_a_gap() {
        let mut tl = crate::testing::media_fixture(&[(0, 100, 20, 400), (150, 100, 20, 400)]);
        let track = tl.tracks()[0].id;
        let ids: Vec<ClipId> = tl.tracks()[0].clips().iter().map(|c| c.id).collect();
        // The first clip has no predecessor; the second is across a gap.
        assert!(
            tl.set_transition(track, ids[0], Some(Transition::of("dissolve")))
                .is_err()
        );
        assert!(
            tl.set_transition(track, ids[1], Some(Transition::of("dissolve")))
                .is_err()
        );
    }

    /// Regression: rippling away a neighbour used to leave
    /// a transition attached to a cut that no longer existed.
    #[test]
    fn removing_a_neighbour_takes_its_transition_with_it() {
        let mut tl = crate::testing::media_fixture(&[
            (0, 100, 20, 400),
            (100, 100, 20, 400),
            (200, 100, 20, 400),
        ]);
        let track = tl.tracks()[0].id;
        let ids: Vec<ClipId> = tl.tracks()[0].clips().iter().map(|c| c.id).collect();
        tl.set_transition(track, ids[1], Some(Transition::of("dissolve")))
            .unwrap();
        tl.set_transition(track, ids[2], Some(Transition::of("dissolve")))
            .unwrap();

        tl.ripple_delete_clip(track, ids[0]).unwrap();

        assert!(
            tl.find_clip(ids[1])
                .is_some_and(|(_, c)| c.transition_in.is_none()),
            "the outgoing clip is gone, so its transition is too"
        );
        assert!(
            tl.find_clip(ids[2])
                .is_some_and(|(_, c)| c.transition_in.is_some()),
            "a transition on an untouched cut survives"
        );
        tl.assert_invariants();
    }

    #[test]
    fn the_nearest_cut_is_what_gx_and_dax_act_on() {
        let mut tl = crate::testing::media_fixture(&[
            (0, 100, 20, 400),
            (100, 100, 20, 400),
            (200, 100, 20, 400),
        ]);
        let track = tl.tracks()[0].id;
        let ids: Vec<ClipId> = tl.tracks()[0].clips().iter().map(|c| c.id).collect();
        assert_eq!(tl.nearest_cut(track, Frame(10)), Some((ids[1], Frame(100))));
        assert_eq!(
            tl.nearest_cut(track, Frame(190)),
            Some((ids[2], Frame(200)))
        );
        assert_eq!(
            tl.nearest_cut(track, Frame(100)),
            Some((ids[1], Frame(100))),
            "sitting on a cut picks that cut"
        );

        assert_eq!(tl.transition_at(track, Frame(100)), None);
        tl.set_transition(track, ids[1], Some(Transition::of("dissolve")))
            .unwrap();
        assert_eq!(
            tl.transition_at(track, Frame(96))
                .map(|(id, t)| (id, t.duration)),
            Some((ids[1], Frame(12)))
        );
    }

    /// `ac` is the clip plus its adjoining transitions, and equals
    /// `ic` until one is attached.
    #[test]
    fn ac_widens_to_cover_adjoining_transitions() {
        let mut tl = crate::testing::media_fixture(&[
            (0, 100, 20, 400),
            (100, 100, 20, 400),
            (200, 100, 20, 400),
        ]);
        let track = tl.tracks()[0].id;
        let ids: Vec<ClipId> = tl.tracks()[0].clips().iter().map(|c| c.id).collect();
        let range = |tl: &Timeline| {
            tl.track(track)
                .and_then(|t| t.transition_range(ids[1]))
                .unwrap()
        };
        assert_eq!(range(&tl), (Frame(100), Frame(200)));
        tl.set_transition(track, ids[1], Some(Transition::of("dissolve")))
            .unwrap();
        tl.set_transition(track, ids[2], Some(Transition::new("dissolve", Frame(7))))
            .unwrap();
        assert_eq!(
            range(&tl),
            (Frame(94), Frame(204)),
            "six frames before the head cut, four past the tail cut"
        );
    }

    #[test]
    fn duration_is_the_longest_track() {
        let tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 250, "b")])]);
        assert_eq!(tl.duration(), Frame(250));
    }
}
