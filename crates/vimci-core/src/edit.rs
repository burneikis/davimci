//! Primitive edit operations (plan.md Phase 1, spec §4).
//!
//! Every primitive is pure model code: no backend, no I/O. Every primitive is
//! validate-then-mutate, so a rejected operation leaves the timeline
//! byte-identical (plan.md Phase 0 rule 1).

use crate::clip::Clip;
use crate::error::CoreError;
use crate::id::{ClipId, TrackId};
use crate::time::Frame;
use crate::timeline::{Register, Timeline};

/// Add a signed frame delta, rejecting anything before frame zero.
pub(crate) fn shift(frame: Frame, delta: i64) -> Result<Frame, CoreError> {
    let v = i128::from(frame.get()) + i128::from(delta);
    if v < 0 {
        return Err(CoreError::NegativeTime);
    }
    Ok(Frame(v as u64))
}

impl Timeline {
    // -- split -----------------------------------------------------------

    /// Split the clip under `frame` on `track` (spec §4, `s`).
    ///
    /// Returns the id of the newly created right-hand clip. Splitting exactly
    /// on a clip boundary is a no-op error: there is nothing to cut.
    pub fn split_at(&mut self, track: TrackId, frame: Frame) -> Result<ClipId, CoreError> {
        // Validate before allocating: a rejected split must not even consume
        // an id, or it would not leave the timeline byte-identical.
        self.check_split(track, frame)?;
        let id = self.new_clip_id();
        self.split_at_with_id(track, frame, id)
    }

    /// Index of the clip a split at `frame` would cut, or a user error.
    fn check_split(&self, track: TrackId, frame: Frame) -> Result<usize, CoreError> {
        let t = self.require_track(track)?;
        let idx = t
            .index_at(frame)
            .ok_or_else(|| CoreError::NoClipAtPlayhead {
                track: t.name.clone(),
                frame: frame.get(),
            })?;
        if t.clips()[idx].start == frame {
            return Err(CoreError::NothingToSplit { frame: frame.get() });
        }
        Ok(idx)
    }

    /// Split, naming the new right-hand clip explicitly.
    ///
    /// Used by the command layer so that redoing a split reproduces the same
    /// clip id and therefore byte-identical state (plan.md Phase 2).
    pub fn split_at_with_id(
        &mut self,
        track: TrackId,
        frame: Frame,
        id: ClipId,
    ) -> Result<ClipId, CoreError> {
        if self.find_clip(id).is_some() {
            return Err(CoreError::DuplicateClip(id.to_string()));
        }
        let idx = self.check_split(track, frame)?;

        let t = self.require_track_mut(track)?;
        let left = &mut t.clips_mut()[idx];
        let mut right = left.clone();
        let head = frame.get() - left.start.get();
        left.duration = Frame(head);
        left.props.fade_out = Frame::ZERO;

        right.id = id;
        right.start = frame;
        right.source_in = Frame(right.source_in.get() + head);
        right.duration = Frame(right.duration.get() - head);
        right.props.fade_in = Frame::ZERO;
        // A split breaks linkage: the halves are no longer the grouped clip.
        right.group = None;
        t.clips_mut().insert(idx + 1, right);

        self.debug_assert_invariants();
        Ok(id)
    }

    /// Split at `frame` only if it falls strictly inside a clip.
    fn split_if_inside(&mut self, track: TrackId, frame: Frame) {
        let _ = self.split_at(track, frame);
    }

    /// Whether `frame` falls strictly inside a clip, i.e. whether an edit at
    /// `frame` will introduce a cut. The command layer needs this to know
    /// which cuts its inverse must join back up.
    #[must_use]
    pub fn cuts_a_clip(&self, track: TrackId, frame: Frame) -> bool {
        self.track(track)
            .and_then(|t| t.clip_at(frame))
            .is_some_and(|c| c.start < frame)
    }

    /// Merge the clip starting at `frame` into its left neighbour - the exact
    /// inverse of [`Timeline::split_at`] (spec §4).
    ///
    /// Only clips that are genuinely two halves of one source window may be
    /// joined: same media, contiguous source, identical properties, and no
    /// linkage or fade caught in the middle. Anything else is a user error,
    /// because joining it would silently discard state.
    ///
    /// Returns the id of the clip that was absorbed.
    pub fn join_at(&mut self, track: TrackId, frame: Frame) -> Result<ClipId, CoreError> {
        let reject = |reason: &str| CoreError::CannotJoin {
            frame: frame.get(),
            reason: reason.to_string(),
        };
        let t = self.require_track(track)?;
        let Some(idx) = t.clips().iter().position(|c| c.start == frame) else {
            return Err(reject("there is no clip starting there"));
        };
        if idx == 0 || t.clips()[idx - 1].end() != frame {
            return Err(reject("there is no clip ending there"));
        }
        let (left, right) = (&t.clips()[idx - 1], &t.clips()[idx]);
        let same_media = match (&left.media, &right.media) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if !same_media {
            return Err(reject("the two clips come from different sources"));
        }
        if left.source_out() != right.source_in {
            return Err(reject("the two clips are not contiguous in the source"));
        }
        if left.label != right.label || left.text != right.text {
            return Err(reject("the two clips are not halves of one clip"));
        }
        if right.group.is_some() {
            return Err(reject("the right-hand clip is linked"));
        }
        if left.props.fade_out != Frame::ZERO || right.props.fade_in != Frame::ZERO {
            return Err(reject("there is a fade at the cut"));
        }
        if left.props.gain_db != right.props.gain_db
            || left.props.transform != right.props.transform
        {
            return Err(reject("the two clips have different properties"));
        }
        let (absorbed, dur, fade_out) = (right.id, right.duration, right.props.fade_out);

        let t = self.require_track_mut(track)?;
        t.clips_mut().remove(idx);
        let left = &mut t.clips_mut()[idx - 1];
        left.duration = Frame(left.duration.get() + dur.get());
        left.props.fade_out = fade_out;
        self.debug_assert_invariants();
        Ok(absorbed)
    }

    /// Put previously removed clips back, ids and linkage intact.
    ///
    /// This is the id-preserving counterpart of [`Timeline::paste`]: paste is
    /// a user action and mints fresh clips, restore is undo and must recreate
    /// exactly what was there. `clips` use register-relative starts, all
    /// within `[0, span)`.
    pub fn restore(
        &mut self,
        track: TrackId,
        at: Frame,
        clips: &[Clip],
        span: Frame,
        ripple: bool,
    ) -> Result<(), CoreError> {
        self.require_track(track)?;
        if span == Frame::ZERO {
            return Err(CoreError::ZeroDuration);
        }
        let mut prev_end = Frame::ZERO;
        for c in clips {
            validate_insertable(c)?;
            if c.start < prev_end || c.end() > span {
                return Err(CoreError::InvalidRange {
                    start: c.start.get(),
                    end: c.end().get(),
                });
            }
            prev_end = c.end();
            if self.find_clip(c.id).is_some() {
                return Err(CoreError::DuplicateClip(c.id.to_string()));
            }
        }

        if ripple {
            self.split_if_inside(track, at);
            let t = self.require_track_mut(track)?;
            for c in t.clips_mut() {
                if c.start >= at {
                    c.start = Frame(c.start.get() + span.get());
                }
            }
        } else {
            self.lift_range(track, at, Frame(at.get() + span.get()))?;
        }
        let t = self.require_track_mut(track)?;
        for c in clips {
            let mut copy = c.clone();
            copy.start = Frame(at.get() + c.start.get());
            t.insert_sorted(copy);
        }
        self.debug_assert_invariants();
        Ok(())
    }

    // -- yank / delete ---------------------------------------------------

    /// Copy `[start, end)` on `track` into a register (spec §4, `y`).
    ///
    /// Partially covered clips are copied trimmed; the timeline is untouched.
    pub fn yank_range(
        &self,
        track: TrackId,
        start: Frame,
        end: Frame,
    ) -> Result<Register, CoreError> {
        let t = self.require_track(track)?;
        validate_range(start, end)?;
        let mut clips = Vec::new();
        for c in t.clips() {
            if c.end() <= start || c.start >= end {
                continue;
            }
            let mut copy = c.clone();
            let head = start.get().saturating_sub(c.start.get());
            let tail = c.end().get().saturating_sub(end.get());
            copy.source_in = Frame(copy.source_in.get() + head);
            copy.duration = Frame(copy.duration.get() - head - tail);
            copy.start = Frame(c.start.get().max(start.get()) - start.get());
            clips.push(copy);
        }
        Ok(Register {
            clips,
            span: Frame(end.get() - start.get()),
        })
    }

    /// Remove `[start, end)` and leave a gap (spec §4, `gd` lift).
    pub fn lift_range(
        &mut self,
        track: TrackId,
        start: Frame,
        end: Frame,
    ) -> Result<Register, CoreError> {
        let yanked = self.yank_range(track, start, end)?;
        self.split_if_inside(track, start);
        self.split_if_inside(track, end);
        let t = self.require_track_mut(track)?;
        t.clips_mut().retain(|c| c.end() <= start || c.start >= end);
        self.debug_assert_invariants();
        Ok(yanked)
    }

    /// Remove `[start, end)` and close the gap (spec §4, `x`/`d`).
    pub fn ripple_delete_range(
        &mut self,
        track: TrackId,
        start: Frame,
        end: Frame,
    ) -> Result<Register, CoreError> {
        let yanked = self.lift_range(track, start, end)?;
        let span = end.get() - start.get();
        let t = self.require_track_mut(track)?;
        for c in t.clips_mut() {
            if c.start >= end {
                c.start = Frame(c.start.get() - span);
            }
        }
        self.debug_assert_invariants();
        Ok(yanked)
    }

    /// Lift one whole clip, leaving a gap.
    pub fn lift_clip(&mut self, track: TrackId, clip: ClipId) -> Result<Register, CoreError> {
        let (s, e) = self.clip_extent(track, clip)?;
        self.lift_range(track, s, e)
    }

    /// Ripple-delete one whole clip (spec §4, `dd`).
    pub fn ripple_delete_clip(
        &mut self,
        track: TrackId,
        clip: ClipId,
    ) -> Result<Register, CoreError> {
        let (s, e) = self.clip_extent(track, clip)?;
        self.ripple_delete_range(track, s, e)
    }

    // -- insert / overwrite / paste --------------------------------------

    /// Insert a clip at `at`, rippling later clips right (spec §4, `i`/`p`).
    ///
    /// A clip straddling `at` is split, so insertion never overwrites.
    pub fn insert_clip(
        &mut self,
        track: TrackId,
        clip: Clip,
        at: Frame,
    ) -> Result<ClipId, CoreError> {
        self.require_track(track)?;
        validate_insertable(&clip)?;
        let id = clip.id;
        let span = clip.duration.get();

        self.split_if_inside(track, at);
        let t = self.require_track_mut(track)?;
        for c in t.clips_mut() {
            if c.start >= at {
                c.start = Frame(c.start.get() + span);
            }
        }
        let mut clip = clip;
        clip.start = at;
        t.insert_sorted(clip);
        self.debug_assert_invariants();
        Ok(id)
    }

    /// Place a clip at `at`, replacing whatever is there (spec §4, `gp`).
    pub fn overwrite_clip(
        &mut self,
        track: TrackId,
        clip: Clip,
        at: Frame,
    ) -> Result<ClipId, CoreError> {
        self.require_track(track)?;
        validate_insertable(&clip)?;
        let id = clip.id;
        let end = Frame(at.get() + clip.duration.get());
        self.lift_range(track, at, end)?;
        let t = self.require_track_mut(track)?;
        let mut clip = clip;
        clip.start = at;
        t.insert_sorted(clip);
        self.debug_assert_invariants();
        Ok(id)
    }

    /// Paste register contents at `at` (spec §4, `p` / `gp`).
    ///
    /// Pasted clips get fresh ids so a register can be pasted repeatedly.
    pub fn paste(
        &mut self,
        track: TrackId,
        at: Frame,
        register: &Register,
        ripple: bool,
    ) -> Result<Vec<ClipId>, CoreError> {
        self.require_track(track)?;
        if register.is_empty() {
            return Err(CoreError::EmptyRegister);
        }
        for c in &register.clips {
            validate_insertable(c)?;
        }

        let span = register.span.get();
        if ripple {
            self.split_if_inside(track, at);
            let t = self.require_track_mut(track)?;
            for c in t.clips_mut() {
                if c.start >= at {
                    c.start = Frame(c.start.get() + span);
                }
            }
        } else {
            self.lift_range(track, at, Frame(at.get() + span))?;
        }

        let mut ids = Vec::with_capacity(register.clips.len());
        let mut fresh = Vec::with_capacity(register.clips.len());
        for c in &register.clips {
            let mut copy = c.clone();
            copy.id = self.new_clip_id();
            copy.start = Frame(at.get() + c.start.get());
            copy.group = None;
            ids.push(copy.id);
            fresh.push(copy);
        }
        let t = self.require_track_mut(track)?;
        for c in fresh {
            t.insert_sorted(c);
        }
        self.debug_assert_invariants();
        Ok(ids)
    }

    /// Move a clip to `new_start` on the same track, overwriting what is
    /// there and leaving a gap behind.
    pub fn move_clip(
        &mut self,
        track: TrackId,
        clip: ClipId,
        new_start: Frame,
    ) -> Result<(), CoreError> {
        let t = self.require_track(track)?;
        let c = t
            .clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?
            .clone();
        validate_insertable(&c)?;
        let t = self.require_track_mut(track)?;
        let Some(idx) = t.index_of(clip) else {
            return Err(CoreError::NoSuchClip(clip.to_string()));
        };
        let original = t.clips_mut().remove(idx);
        let mut moved = c;
        moved.group = None;
        if let Err(e) = self.overwrite_clip(track, moved, new_start) {
            // The clip is already out of the track, so a late rejection has
            // to put it back: a refused move must leave the timeline
            // byte-identical (plan.md Phase 0 rule 1).
            if let Ok(t) = self.require_track_mut(track) {
                t.insert_sorted(original);
            }
            return Err(e);
        }
        self.debug_assert_invariants();
        Ok(())
    }

    // -- helpers ---------------------------------------------------------

    pub(crate) fn clip_extent(
        &self,
        track: TrackId,
        clip: ClipId,
    ) -> Result<(Frame, Frame), CoreError> {
        let t = self.require_track(track)?;
        let c = t
            .clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        Ok((c.start, c.end()))
    }
}

fn validate_range(start: Frame, end: Frame) -> Result<(), CoreError> {
    if end <= start {
        return Err(CoreError::InvalidRange {
            start: start.get(),
            end: end.get(),
        });
    }
    Ok(())
}

fn validate_insertable(clip: &Clip) -> Result<(), CoreError> {
    if clip.duration == Frame::ZERO {
        return Err(CoreError::ZeroDuration);
    }
    if let Some(m) = &clip.media
        && clip.source_out() > m.length
    {
        return Err(CoreError::InsufficientHandles {
            shortfall: clip.source_out().get() - m.length.get(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testing::{clip_ids, fixture, track_id};

    #[test]
    fn split_creates_two_adjacent_clips() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let right = tl.split_at(v1, Frame(40)).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-40][a 40-100]\nA1: -\n");
        assert_eq!(tl.find_clip(right).map(|(_, c)| c.start), Some(Frame(40)));
        tl.assert_invariants();
    }

    #[test]
    fn split_carries_the_source_in_point_forward() {
        let mut tl = crate::testing::media_fixture(&[(0, 100, 20, 300)]);
        let v1 = track_id(&tl, "V1");
        let right = tl.split_at(v1, Frame(40)).unwrap();
        let (_, r) = tl.find_clip(right).unwrap();
        assert_eq!(r.source_in, Frame(60));
        assert_eq!(r.duration, Frame(60));
    }

    #[test]
    fn split_on_a_boundary_is_rejected_and_changes_nothing() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 50, "b")])]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        assert!(tl.split_at(v1, Frame(100)).is_err());
        assert!(tl.split_at(v1, Frame(999)).is_err());
        assert_eq!(tl, before);
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 150, "b")])]);
        let v1 = track_id(&tl, "V1");
        tl.ripple_delete_range(v1, Frame(40), Frame(120)).unwrap();
        // b keeps its remaining 130 frames and slides left to close the gap.
        assert_eq!(tl.dump(), "V1:[a 0-40][b 40-170]\nA1: -\n");
        tl.assert_invariants();
    }

    #[test]
    fn lift_leaves_the_gap() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 150, "b")])]);
        let v1 = track_id(&tl, "V1");
        tl.lift_range(v1, Frame(40), Frame(120)).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-40]<gap 80>[b 120-250]\nA1: -\n");
    }

    #[test]
    fn ripple_delete_a_whole_clip() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 50, "b"), (150, 50, "c")])]);
        let v1 = track_id(&tl, "V1");
        let b = clip_ids(&tl, "V1")[1];
        tl.ripple_delete_clip(v1, b).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-100][c 100-150]\nA1: -\n");
    }

    #[test]
    fn yank_does_not_mutate_and_rebases_to_zero() {
        let tl = fixture(&[("V1", &[(0, 100, "a"), (100, 150, "b")])]);
        let v1 = track_id(&tl, "V1");
        let reg = tl.yank_range(v1, Frame(40), Frame(120)).unwrap();
        assert_eq!(reg.span, Frame(80));
        assert_eq!(reg.clips.len(), 2);
        assert_eq!(reg.clips[0].start, Frame::ZERO);
        assert_eq!(reg.clips[0].duration, Frame(60));
        assert_eq!(reg.clips[1].start, Frame(60));
        assert_eq!(reg.clips[1].duration, Frame(20));
    }

    #[test]
    fn empty_and_inverted_ranges_are_user_errors() {
        let tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        assert!(tl.yank_range(v1, Frame(50), Frame(50)).is_err());
        assert!(tl.yank_range(v1, Frame(60), Frame(50)).is_err());
    }

    #[test]
    fn insert_ripples_and_splits() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let id = tl.new_clip_id();
        let clip = Clip::generated(id, "n", Frame::ZERO, Frame(30));
        tl.insert_clip(v1, clip, Frame(40)).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-40][n 40-70][a 70-130]\nA1: -\n");
    }

    #[test]
    fn overwrite_replaces_the_covered_range() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let id = tl.new_clip_id();
        let clip = Clip::generated(id, "n", Frame::ZERO, Frame(30));
        tl.overwrite_clip(v1, clip, Frame(40)).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-40][n 40-70][a 70-100]\nA1: -\n");
    }

    #[test]
    fn paste_ripple_and_overwrite_differ_only_in_shifting() {
        let src = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&src, "V1");
        let reg = src.yank_range(v1, Frame(0), Frame(20)).unwrap();

        let mut a = fixture(&[("V1", &[(0, 60, "x")])]);
        a.paste(track_id(&a, "V1"), Frame(30), &reg, true).unwrap();
        assert_eq!(a.dump(), "V1:[x 0-30][a 30-50][x 50-80]\nA1: -\n");

        let mut b = fixture(&[("V1", &[(0, 60, "x")])]);
        b.paste(track_id(&b, "V1"), Frame(30), &reg, false).unwrap();
        assert_eq!(b.dump(), "V1:[x 0-30][a 30-50][x 50-60]\nA1: -\n");
    }

    #[test]
    fn pasted_clips_get_fresh_ids() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let reg = tl.yank_range(v1, Frame(0), Frame(50)).unwrap();
        let ids = tl.paste(v1, Frame(100), &reg, true).unwrap();
        assert_eq!(ids.len(), 1);
        assert_ne!(ids[0], reg.clips[0].id);
    }

    #[test]
    fn pasting_an_empty_register_is_rejected() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let before = tl.clone();
        assert!(tl.paste(v1, Frame(0), &Register::default(), true).is_err());
        assert_eq!(tl, before);
    }

    #[test]
    fn move_clip_leaves_a_gap_and_overwrites_the_target() {
        let mut tl = fixture(&[("V1", &[(0, 50, "a"), (100, 50, "b")])]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        tl.move_clip(v1, a, Frame(120)).unwrap();
        assert_eq!(tl.dump(), "V1:<gap 100>[b 100-120][a 120-170]\nA1: -\n");
    }

    #[test]
    fn a_zero_duration_clip_cannot_be_inserted() {
        let mut tl = fixture(&[("V1", &[(0, 50, "a")])]);
        let v1 = track_id(&tl, "V1");
        let before = tl.clone();
        let id = tl.new_clip_id();
        let clip = Clip::generated(id, "n", Frame::ZERO, Frame::ZERO);
        assert!(tl.insert_clip(v1, clip, Frame(0)).is_err());
        assert_eq!(tl.tracks(), before.tracks());
    }

    #[test]
    fn operations_on_an_unknown_track_are_rejected() {
        let mut tl = fixture(&[("V1", &[(0, 50, "a")])]);
        let ghost = TrackId(9999);
        assert!(tl.split_at(ghost, Frame(10)).is_err());
        assert!(tl.ripple_delete_range(ghost, Frame(0), Frame(5)).is_err());
        assert!(tl.yank_range(ghost, Frame(0), Frame(5)).is_err());
    }

    #[test]
    fn join_is_the_exact_inverse_of_split() {
        let mut tl = crate::testing::media_fixture(&[(0, 100, 20, 300), (100, 60, 0, 300)]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        let right = tl.split_at(v1, Frame(40)).unwrap();
        let absorbed = tl.join_at(v1, Frame(40)).unwrap();
        assert_eq!(absorbed, right);
        assert_eq!(tl.tracks(), before.tracks());
    }

    #[test]
    fn join_refuses_unrelated_neighbours() {
        let mut tl = crate::testing::media_fixture(&[(0, 100, 0, 300), (100, 60, 0, 300)]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        assert!(matches!(
            tl.join_at(v1, Frame(100)),
            Err(CoreError::CannotJoin { .. })
        ));
        assert!(tl.join_at(v1, Frame(0)).is_err());
        assert!(tl.join_at(v1, Frame(50)).is_err());
        assert_eq!(tl, before);
    }

    #[test]
    fn join_refuses_to_swallow_a_fade_or_a_link() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let right = tl.split_at(v1, Frame(40)).unwrap();
        let mut props = tl.find_clip(right).map(|(_, c)| c.props).unwrap();
        props.fade_in = Frame(5);
        tl.set_clip_props(v1, right, props).unwrap();
        assert!(tl.join_at(v1, Frame(40)).is_err());
    }

    #[test]
    fn restore_puts_back_the_exact_clips_that_were_removed() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 150, "b")])]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        let reg = tl.ripple_delete_range(v1, Frame(100), Frame(250)).unwrap();
        tl.restore(v1, Frame(100), &reg.clips, reg.span, true)
            .unwrap();
        assert_eq!(tl.tracks(), before.tracks());
    }

    #[test]
    fn restore_rejects_an_id_that_is_already_present() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let reg = tl.yank_range(v1, Frame(0), Frame(100)).unwrap();
        let before = tl.clone();
        assert!(matches!(
            tl.restore(v1, Frame(100), &reg.clips, reg.span, true),
            Err(CoreError::DuplicateClip(_))
        ));
        assert_eq!(tl, before);
    }

    #[test]
    fn restore_into_a_gap_does_not_ripple() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        let reg = tl.lift_range(v1, Frame(100), Frame(200)).unwrap();
        assert_eq!(tl.dump(), "V1:[a 0-100]\nA1: -\n");
        tl.restore(v1, Frame(100), &reg.clips, reg.span, false)
            .unwrap();
        assert_eq!(tl.tracks(), before.tracks());
    }

    #[test]
    fn a_split_can_be_given_its_id() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let id = tl.new_clip_id();
        assert_eq!(tl.split_at_with_id(v1, Frame(40), id), Ok(id));
        let existing = clip_ids(&tl, "V1")[0];
        assert!(matches!(
            tl.split_at_with_id(v1, Frame(20), existing),
            Err(CoreError::DuplicateClip(_))
        ));
    }

    #[test]
    fn cuts_a_clip_reports_where_an_edit_will_split() {
        let tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        assert!(tl.cuts_a_clip(v1, Frame(50)));
        assert!(!tl.cuts_a_clip(v1, Frame(0)));
        assert!(!tl.cuts_a_clip(v1, Frame(100)));
    }

    #[test]
    fn shift_rejects_negative_time() {
        assert!(shift(Frame(5), -10).is_err());
        assert_eq!(shift(Frame(5), -5), Ok(Frame::ZERO));
        assert_eq!(shift(Frame(5), 5), Ok(Frame(10)));
    }
}
