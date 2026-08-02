//! The timeline: tracks, playhead, markers, marks, registers, invariants.
//!
//! Mutation lives in [`crate::edit`] and [`crate::trim`]. Everything here is
//! construction, query, and verification.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::clip::Clip;
use crate::error::CoreError;
use crate::id::{ClipId, GroupId, IdGen, TrackId};
use crate::time::{Frame, TimelineProps};
use crate::track::{Track, TrackKind};

/// A named point on the timeline (spec §3.2 jump-point source).
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

/// Playhead position: a frame plus the focused track (spec §5).
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
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let id = self.ids.track();
        self.tracks
            .push(Track::new(id, format!("{}{n}", kind.prefix()), kind));
        id
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

    // -- grouping (spec §5) ----------------------------------------------

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

    /// Remove one clip from its group (spec §5: `:unlink`).
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

    /// Full structural check (plan.md Phase 1 invariant list).
    pub fn check_invariants(&self) -> Result<(), CoreError> {
        for t in &self.tracks {
            t.check_invariants()?;
        }
        // Group members stay frame-aligned.
        let mut groups: BTreeMap<GroupId, (Frame, Frame)> = BTreeMap::new();
        for t in &self.tracks {
            for c in t.clips() {
                if let Some(g) = c.group {
                    match groups.get(&g) {
                        None => {
                            groups.insert(g, (c.start, c.end()));
                        }
                        Some(&(s, e)) if s == c.start && e == c.end() => {}
                        Some(_) => {
                            return Err(CoreError::InvariantViolation(format!(
                                "group {g} is not frame-aligned at clip {}",
                                c.id
                            )));
                        }
                    }
                }
            }
        }
        if self.track(self.playhead.track).is_none() {
            return Err(CoreError::InvariantViolation(
                "playhead focuses a track that does not exist".into(),
            ));
        }
        Ok(())
    }

    /// Panicking form, for use at the end of every mutation in tests and
    /// debug builds. The only sanctioned panic path (plan.md Phase 0).
    #[allow(clippy::panic)]
    pub fn assert_invariants(&self) {
        if let Err(e) = self.check_invariants() {
            crate::assert_invariant!(false, "{e}");
        }
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
    fn added_tracks_are_numbered_per_kind() {
        let mut tl = Timeline::new(TimelineProps::default());
        tl.add_track(TrackKind::Audio);
        tl.add_track(TrackKind::Text);
        assert!(tl.track_by_name("A2").is_some());
        assert!(tl.track_by_name("T1").is_some());
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
            .map(|t| t.clips()[0].id)
            .unwrap_or(crate::id::ClipId(0));
        let a = tl
            .track_by_name("A1")
            .map(|t| t.clips()[0].id)
            .unwrap_or(crate::id::ClipId(0));
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
    fn duration_is_the_longest_track() {
        let tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 250, "b")])]);
        assert_eq!(tl.duration(), Frame(250));
    }
}
