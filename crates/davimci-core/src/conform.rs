//! Re-conform: changing the timeline's framerate with clips already on it
//! (spec 7.1, plan.md Phase 5).
//!
//! The timeline has exactly one framerate. Changing it retimes every clip,
//! marker, mark, register and the playhead, so there is still one and only
//! one notion of "frame N". Nearest-frame mapping is computed independently
//! per boundary from [`Fps::conform_frame`], so error never accumulates.
//!
//! Rounding can collapse a short clip to zero frames or overlap two
//! neighbours, both of which break an invariant, so the retime is computed
//! into a candidate and validated before anything is committed. A collapsed
//! clip is repaired rather than rejected: it keeps one frame and its
//! neighbours are pushed along.
//!
//! Repair is lossy, so the inverse of a re-conform is not another re-conform.
//! [`Timeline::reconform`] hands back the exact prior geometry and
//! [`Timeline::restore_conform`] puts it back verbatim, which is what makes
//! `:set timeline.fps` exactly invertible.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::clip::Clip;
use crate::error::CoreError;
use crate::id::TrackId;
use crate::time::{Fps, Frame, TimelineProps};
use crate::timeline::{Mark, Marker, Register, Timeline};

/// A complete snapshot of everything a re-conform touches.
///
/// Held by the command layer as the inverse of a re-conform, so undo is
/// exact rather than "conform back and hope the rounding agrees".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConformState {
    pub props: TimelineProps,
    pub tracks: Vec<(TrackId, Vec<Clip>)>,
    pub playhead: Frame,
    pub markers: Vec<Marker>,
    pub marks: BTreeMap<char, Mark>,
    pub registers: BTreeMap<char, Register>,
}

impl Timeline {
    /// Capture everything [`Timeline::reconform`] would overwrite.
    #[must_use]
    pub fn conform_state(&self) -> ConformState {
        ConformState {
            props: self.props,
            tracks: self
                .tracks()
                .iter()
                .map(|t| (t.id, t.clips().to_vec()))
                .collect(),
            playhead: self.playhead().frame,
            markers: self.markers.clone(),
            marks: self.marks.clone(),
            registers: self.registers.clone(),
        }
    }

    /// Retime the whole timeline to `props` (spec 7.1).
    ///
    /// Returns the state as it was, which is the only exact inverse.
    pub fn reconform(&mut self, props: TimelineProps) -> Result<ConformState, CoreError> {
        let before = self.conform_state();
        if props.fps == self.props.fps {
            // Resolution and sample rate are render-time properties: nothing
            // on the timeline is measured in either.
            self.props = props;
            self.debug_assert_invariants();
            return Ok(before);
        }

        let (from, to) = (self.props.fps, props.fps);
        let mut candidate = self.clone();
        candidate.props = props;
        for (id, clips) in &before.tracks {
            let retimed = conform_clips(clips, from, to);
            let t = candidate.require_track_mut(*id)?;
            *t.clips_mut() = retimed;
        }
        candidate.set_playhead_frame(to.conform_frame(before.playhead, from));
        for m in &mut candidate.markers {
            m.frame = to.conform_frame(m.frame, from);
        }
        for m in candidate.marks.values_mut() {
            m.frame = to.conform_frame(m.frame, from);
        }
        for r in candidate.registers.values_mut() {
            r.clips = conform_clips(&r.clips, from, to);
            r.span = to
                .conform_frame(r.span, from)
                .max(r.clips.last().map_or(Frame::ZERO, Clip::end));
        }

        // Validate before committing: a re-conform that would corrupt the
        // model is a user error, not a crash.
        candidate.check_invariants()?;
        *self = candidate;
        self.debug_assert_invariants();
        Ok(before)
    }

    /// Put back a state captured by [`Timeline::reconform`], verbatim.
    ///
    /// Returns the state it replaced, so undo and redo are symmetric.
    pub fn restore_conform(&mut self, state: &ConformState) -> Result<ConformState, CoreError> {
        let before = self.conform_state();
        let mut candidate = self.clone();
        candidate.props = state.props;
        for (id, clips) in &state.tracks {
            let t = candidate.require_track_mut(*id)?;
            *t.clips_mut() = clips.clone();
        }
        candidate.set_playhead_frame(state.playhead);
        candidate.markers = state.markers.clone();
        candidate.marks = state.marks.clone();
        candidate.registers = state.registers.clone();
        candidate.check_invariants()?;
        *self = candidate;
        self.debug_assert_invariants();
        Ok(before)
    }
}

/// Retime one track's clips, repairing collapses and overlaps in order.
fn conform_clips(clips: &[Clip], from: Fps, to: Fps) -> Vec<Clip> {
    let mut out: Vec<Clip> = Vec::with_capacity(clips.len());
    let mut floor = Frame::ZERO;
    for c in clips {
        let mut next = c.clone();
        let start = to.conform_frame(c.start, from).max(floor);
        // A clip must survive the retime: a sub-frame clip keeps one frame
        // rather than vanishing, which would lose content silently.
        let end = to
            .conform_frame(c.end(), from)
            .max(Frame(start.get().saturating_add(1)));
        next.start = start;
        next.duration = Frame(end.get() - start.get());
        next.source_in = to.conform_frame(c.source_in, from);
        next.props.fade_in = to.conform_frame(c.props.fade_in, from).min(next.duration);
        next.props.fade_out = to.conform_frame(c.props.fade_out, from).min(next.duration);
        let source_out = next.source_out();
        if let Some(m) = next.media.as_mut() {
            // The source did not change length; only its expression in
            // timeline frames did. It must still cover the clip's window.
            m.length = to.conform_frame(m.length, from).max(source_out);
        }
        floor = end;
        out.push(next);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testing::{fixture, media_fixture, track_id};
    use crate::time::Resolution;

    fn props(fps: Fps) -> TimelineProps {
        TimelineProps {
            fps,
            resolution: Resolution::HD_1080,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn doubling_the_rate_doubles_every_boundary() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 150, "b")])]);
        tl.props = props(Fps::FPS_30);
        tl.reconform(props(Fps::FPS_60)).unwrap();
        assert_eq!(tl.dump().lines().next().unwrap(), "V1:[a 0-200][b 200-500]");
        assert_eq!(tl.props.fps, Fps::FPS_60);
    }

    #[test]
    fn a_resolution_change_leaves_time_alone() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let before = tl.dump();
        tl.reconform(TimelineProps {
            resolution: Resolution {
                width: 3840,
                height: 2160,
            },
            ..tl.props
        })
        .unwrap();
        assert_eq!(tl.dump(), before);
        assert_eq!(tl.props.resolution.width, 3840);
    }

    #[test]
    fn undo_of_a_lossy_reconform_is_exact() {
        // 60 -> 23.976 rounds hard, and a two-frame clip collapses.
        let mut tl = fixture(&[("V1", &[(0, 2, "a"), (2, 3, "b"), (5, 1000, "c")])]);
        let before = serde_json::to_string(&tl).unwrap();
        let state = tl.reconform(props(Fps::FPS_23_976)).unwrap();
        assert_ne!(serde_json::to_string(&tl).unwrap(), before);
        tl.restore_conform(&state).unwrap();
        assert_eq!(serde_json::to_string(&tl).unwrap(), before);
    }

    #[test]
    fn no_clip_is_ever_lost_to_rounding() {
        let mut tl = fixture(&[("V1", &[(0, 1, "a"), (1, 1, "b"), (2, 1, "c")])]);
        tl.reconform(props(Fps::FPS_23_976)).unwrap();
        let v1 = track_id(&tl, "V1");
        assert_eq!(tl.track(v1).unwrap().clips().len(), 3);
        tl.assert_invariants();
    }

    #[test]
    fn media_handles_survive_the_retime() {
        let mut tl = media_fixture(&[(0, 100, 50, 300)]);
        tl.props = props(Fps::FPS_30);
        tl.reconform(props(Fps::FPS_60)).unwrap();
        let v1 = track_id(&tl, "V1");
        let c = &tl.track(v1).unwrap().clips()[0];
        assert_eq!(c.duration, Frame(200));
        assert_eq!(c.source_in, Frame(100));
        assert_eq!(c.media.as_ref().unwrap().length, Frame(600));
        tl.assert_invariants();
    }

    #[test]
    fn markers_marks_and_the_playhead_move_with_the_clips() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        tl.props = props(Fps::FPS_30);
        tl.markers.push(Marker {
            frame: Frame(50),
            label: "m".into(),
        });
        tl.marks.insert(
            'a',
            Mark {
                frame: Frame(20),
                track: None,
            },
        );
        tl.set_playhead_frame(Frame(10));
        tl.reconform(props(Fps::FPS_60)).unwrap();
        assert_eq!(tl.markers[0].frame, Frame(100));
        assert_eq!(tl.marks[&'a'].frame, Frame(40));
        assert_eq!(tl.playhead().frame, Frame(20));
    }

    #[test]
    fn registers_are_retimed_so_a_later_paste_is_still_frame_exact() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        tl.props = props(Fps::FPS_30);
        let v1 = track_id(&tl, "V1");
        let reg = tl.yank_range(v1, Frame(0), Frame(100)).unwrap();
        tl.registers.insert('"', reg);
        tl.reconform(props(Fps::FPS_60)).unwrap();
        let reg = &tl.registers[&'"'];
        assert_eq!(reg.span, Frame(200));
        assert_eq!(reg.clips[0].duration, Frame(200));
    }

    #[test]
    fn linked_clips_stay_aligned_across_a_retime() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "b")])]);
        tl.props = props(Fps::FPS_30);
        let ids: Vec<_> = tl
            .tracks()
            .iter()
            .filter_map(|t| t.clips().first().map(|c| c.id))
            .collect();
        tl.link(&ids).unwrap();
        tl.reconform(props(Fps::FPS_23_976)).unwrap();
        tl.assert_invariants();
    }
}
