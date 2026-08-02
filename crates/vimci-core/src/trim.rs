//! The trim family: ripple trim, roll, slip, slide (spec §4.0.1).
//!
//! All four are validate-then-mutate and all four respect source handles: a
//! trim that would run past the end of the media is a user error, rejected
//! before anything moves (plan.md Phase 0).

use serde::{Deserialize, Serialize};

use crate::clip::Clip;
use crate::edit::shift;
use crate::error::CoreError;
use crate::id::{ClipId, TrackId};
use crate::time::Frame;
use crate::timeline::Timeline;

/// Which edge of a clip a trim acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    /// The clip's in-point.
    Head,
    /// The clip's out-point.
    Tail,
}

/// Positive `delta` always means "later in time" for both edges.
fn new_duration(clip: &Clip, edge: Edge, delta: i64) -> Result<Frame, CoreError> {
    let d = match edge {
        Edge::Head => i128::from(clip.duration.get()) - i128::from(delta),
        Edge::Tail => i128::from(clip.duration.get()) + i128::from(delta),
    };
    if d <= 0 {
        return Err(CoreError::ZeroDuration);
    }
    Ok(Frame(d as u64))
}

/// Check the source has enough frames for the requested edge movement.
fn check_handle(clip: &Clip, edge: Edge, delta: i64) -> Result<(), CoreError> {
    let needed = match edge {
        Edge::Head if delta < 0 => delta.unsigned_abs(),
        Edge::Tail if delta > 0 => delta.unsigned_abs(),
        _ => return Ok(()),
    };
    let available = match edge {
        Edge::Head => clip.head_handle(),
        Edge::Tail => clip.tail_handle(),
    };
    match available {
        // Generated clips have no source to run out of.
        None => Ok(()),
        Some(have) if have >= needed => Ok(()),
        Some(have) => Err(CoreError::InsufficientHandles {
            shortfall: needed - have,
        }),
    }
}

impl Timeline {
    /// Ripple trim (spec §4.0.1, `t`): move one edge, shift later clips so no
    /// gap opens. Trimming the head moves the in-point; the clip stays put.
    pub fn ripple_trim(
        &mut self,
        track: TrackId,
        clip: ClipId,
        edge: Edge,
        delta: i64,
    ) -> Result<(), CoreError> {
        if delta == 0 {
            return Ok(());
        }
        let t = self.require_track(track)?;
        let idx = t
            .index_of(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        let c = &t.clips()[idx];
        let dur = new_duration(c, edge, delta)?;
        check_handle(c, edge, delta)?;
        let new_in = match edge {
            Edge::Head => shift(c.source_in, delta)?,
            Edge::Tail => c.source_in,
        };

        let t = self.require_track_mut(track)?;
        {
            let c = &mut t.clips_mut()[idx];
            c.source_in = new_in;
            c.duration = dur;
        }
        let ripple = match edge {
            Edge::Head => -delta,
            Edge::Tail => delta,
        };
        for c in t.clips_mut().iter_mut().skip(idx + 1) {
            c.start = shift(c.start, ripple)?;
        }
        self.debug_assert_invariants();
        Ok(())
    }

    /// Roll (spec §4.0.1, `gt`): move a cut point; both neighbours absorb the
    /// change, so total duration is unchanged.
    pub fn roll(&mut self, track: TrackId, cut: Frame, delta: i64) -> Result<(), CoreError> {
        if delta == 0 {
            return Ok(());
        }
        let t = self.require_track(track)?;
        let Some(right_idx) = t.clips().iter().position(|c| c.start == cut) else {
            return Err(CoreError::NoCutAt { frame: cut.get() });
        };
        if right_idx == 0 || t.clips()[right_idx - 1].end() != cut {
            return Err(CoreError::NoCutAt { frame: cut.get() });
        }
        let left = &t.clips()[right_idx - 1];
        let right = &t.clips()[right_idx];
        let left_dur = new_duration(left, Edge::Tail, delta)?;
        let right_dur = new_duration(right, Edge::Head, delta)?;
        check_handle(left, Edge::Tail, delta)?;
        check_handle(right, Edge::Head, delta)?;
        let right_start = shift(right.start, delta)?;
        let right_in = shift(right.source_in, delta)?;

        let t = self.require_track_mut(track)?;
        t.clips_mut()[right_idx - 1].duration = left_dur;
        let r = &mut t.clips_mut()[right_idx];
        r.start = right_start;
        r.source_in = right_in;
        r.duration = right_dur;
        self.debug_assert_invariants();
        Ok(())
    }

    /// Slip (spec §4.0.1, `T`): change a clip's source in/out points without
    /// moving it or changing its duration.
    pub fn slip(&mut self, track: TrackId, clip: ClipId, delta: i64) -> Result<(), CoreError> {
        if delta == 0 {
            return Ok(());
        }
        let t = self.require_track(track)?;
        let c = t
            .clip(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        // Slipping later consumes tail handle, earlier consumes head handle.
        let needed_head = if delta < 0 { delta.unsigned_abs() } else { 0 };
        let needed_tail = if delta > 0 { delta.unsigned_abs() } else { 0 };
        if let (Some(head), Some(tail)) = (c.head_handle(), c.tail_handle()) {
            if head < needed_head {
                return Err(CoreError::InsufficientHandles {
                    shortfall: needed_head - head,
                });
            }
            if tail < needed_tail {
                return Err(CoreError::InsufficientHandles {
                    shortfall: needed_tail - tail,
                });
            }
        }
        let new_in = shift(c.source_in, delta)?;

        let t = self.require_track_mut(track)?;
        if let Some(c) = t.clip_mut(clip) {
            c.source_in = new_in;
        }
        self.debug_assert_invariants();
        Ok(())
    }

    /// Slide (spec §4.0.1, `gT`): move a clip along the timeline; the
    /// adjacent clips absorb the movement. Both neighbours must be adjacent.
    pub fn slide(&mut self, track: TrackId, clip: ClipId, delta: i64) -> Result<(), CoreError> {
        if delta == 0 {
            return Ok(());
        }
        let t = self.require_track(track)?;
        let idx = t
            .index_of(clip)
            .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
        if idx == 0 || idx + 1 >= t.clips().len() {
            return Err(CoreError::CannotSlide {
                reason: "a slide needs an adjacent clip on both sides".into(),
            });
        }
        let (prev, this, next) = (&t.clips()[idx - 1], &t.clips()[idx], &t.clips()[idx + 1]);
        if prev.end() != this.start || this.end() != next.start {
            return Err(CoreError::CannotSlide {
                reason: "a slide needs an adjacent clip on both sides".into(),
            });
        }
        let prev_dur = new_duration(prev, Edge::Tail, delta)?;
        let next_dur = new_duration(next, Edge::Head, delta)?;
        check_handle(prev, Edge::Tail, delta)?;
        check_handle(next, Edge::Head, delta)?;
        let this_start = shift(this.start, delta)?;
        let next_start = shift(next.start, delta)?;
        let next_in = shift(next.source_in, delta)?;

        let t = self.require_track_mut(track)?;
        t.clips_mut()[idx - 1].duration = prev_dur;
        t.clips_mut()[idx].start = this_start;
        let n = &mut t.clips_mut()[idx + 1];
        n.start = next_start;
        n.source_in = next_in;
        n.duration = next_dur;
        self.debug_assert_invariants();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testing::{clip_ids, fixture, media_fixture, track_id};

    #[test]
    fn ripple_trim_tail_shifts_later_clips() {
        let mut tl = media_fixture(&[(0, 100, 0, 300), (100, 100, 0, 300)]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        tl.ripple_trim(v1, a, Edge::Tail, -30).unwrap();
        assert_eq!(tl.dump(), "V1:[m0 0-70][m1 70-170]\nA1: -\n");
        tl.assert_invariants();
    }

    #[test]
    fn ripple_trim_head_moves_the_in_point_not_the_start() {
        let mut tl = media_fixture(&[(0, 100, 50, 300), (100, 100, 0, 300)]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        tl.ripple_trim(v1, a, Edge::Head, 20).unwrap();
        let (_, c) = tl.find_clip(a).unwrap();
        assert_eq!(c.start, Frame::ZERO);
        assert_eq!(c.source_in, Frame(70));
        assert_eq!(c.duration, Frame(80));
        assert_eq!(tl.dump(), "V1:[m0 0-80][m1 80-180]\nA1: -\n");
    }

    #[test]
    fn trimming_past_the_handles_is_rejected_intact() {
        let mut tl = media_fixture(&[(0, 100, 0, 120)]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        // Only 20 tail frames exist.
        let err = tl.ripple_trim(v1, a, Edge::Tail, 50).unwrap_err();
        assert_eq!(err, CoreError::InsufficientHandles { shortfall: 30 });
        assert_eq!(tl, before);
        // Head has no handle at all.
        assert!(tl.ripple_trim(v1, a, Edge::Head, -1).is_err());
        assert_eq!(tl, before);
    }

    #[test]
    fn trimming_a_clip_to_nothing_is_rejected() {
        let mut tl = media_fixture(&[(0, 100, 0, 300)]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        assert_eq!(
            tl.ripple_trim(v1, a, Edge::Tail, -100),
            Err(CoreError::ZeroDuration)
        );
    }

    #[test]
    fn generated_clips_trim_without_handle_limits() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        tl.ripple_trim(v1, a, Edge::Tail, 10_000).unwrap();
        assert_eq!(tl.duration(), Frame(10_100));
    }

    #[test]
    fn roll_preserves_total_duration() {
        let mut tl = media_fixture(&[(0, 100, 50, 300), (100, 100, 50, 300)]);
        let v1 = track_id(&tl, "V1");
        let total = tl.duration();
        tl.roll(v1, Frame(100), 25).unwrap();
        assert_eq!(tl.duration(), total);
        assert_eq!(tl.dump(), "V1:[m0 0-125][m1 125-200]\nA1: -\n");
        tl.assert_invariants();
    }

    #[test]
    fn roll_needs_a_real_cut() {
        let mut tl = media_fixture(&[(0, 100, 0, 300), (150, 100, 0, 300)]);
        let v1 = track_id(&tl, "V1");
        assert!(matches!(
            tl.roll(v1, Frame(150), 10),
            Err(CoreError::NoCutAt { .. })
        ));
        assert!(matches!(
            tl.roll(v1, Frame(0), 10),
            Err(CoreError::NoCutAt { .. })
        ));
    }

    #[test]
    fn slip_moves_the_source_window_only() {
        let mut tl = media_fixture(&[(0, 100, 50, 300)]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        tl.slip(v1, a, 30).unwrap();
        let (_, c) = tl.find_clip(a).unwrap();
        assert_eq!(c.start, Frame::ZERO);
        assert_eq!(c.duration, Frame(100));
        assert_eq!(c.source_in, Frame(80));
    }

    #[test]
    fn slip_past_the_head_handle_is_rejected() {
        let mut tl = media_fixture(&[(0, 100, 10, 300)]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        assert!(tl.slip(v1, a, -20).is_err());
        assert_eq!(tl, before);
    }

    #[test]
    fn slide_moves_the_clip_and_neighbours_absorb() {
        let mut tl = media_fixture(&[(0, 100, 50, 300), (100, 100, 50, 300), (200, 100, 50, 300)]);
        let v1 = track_id(&tl, "V1");
        let b = clip_ids(&tl, "V1")[1];
        let total = tl.duration();
        tl.slide(v1, b, 20).unwrap();
        assert_eq!(tl.duration(), total);
        assert_eq!(tl.dump(), "V1:[m0 0-120][m1 120-220][m2 220-300]\nA1: -\n");
    }

    #[test]
    fn slide_needs_neighbours_on_both_sides() {
        let mut tl = media_fixture(&[(0, 100, 50, 300), (100, 100, 50, 300)]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        assert!(matches!(
            tl.slide(v1, a, 10),
            Err(CoreError::CannotSlide { .. })
        ));
    }

    #[test]
    fn a_zero_delta_trim_is_a_no_op() {
        let mut tl = media_fixture(&[(0, 100, 50, 300), (100, 100, 50, 300)]);
        let before = tl.clone();
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        assert!(tl.ripple_trim(v1, a, Edge::Tail, 0).is_ok());
        assert!(tl.slip(v1, a, 0).is_ok());
        assert!(tl.slide(v1, a, 0).is_ok());
        assert!(tl.roll(v1, Frame(100), 0).is_ok());
        assert_eq!(tl, before);
    }
}
