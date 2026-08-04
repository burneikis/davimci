//! Property tests for the jump-point engine and motion resolution.

use davimci_core::testing::{fixture, track_id};
use davimci_core::{Frame, Marker, Timeline};
use proptest::prelude::*;

use crate::jump::{JumpConfig, JumpPoints, Zoom};
use crate::motion::{BuiltinMotion as M, Motion, MotionCtx};
use crate::target::Direction;

/// `(duration, gap)` pairs become consecutive clips on `V1`.
fn timeline(spans: &[(u64, u64)], markers: &[u64]) -> Timeline {
    let mut clips: Vec<(u64, u64, &str)> = Vec::new();
    let mut at = 0u64;
    for (dur, gap) in spans {
        clips.push((at, *dur, "c"));
        at += dur + gap;
    }
    let mut tl = fixture(&[("V1", &clips)]);
    let end = tl.duration().get().max(1);
    for m in markers {
        tl.markers.push(Marker {
            frame: Frame(m % end),
            label: "m".into(),
        });
    }
    tl
}

fn spans() -> impl Strategy<Value = Vec<(u64, u64)>> {
    prop::collection::vec((1u64..500, 0u64..200), 1..8)
}

proptest! {
    /// The point set is sorted, unique, and inside the timeline - the
    /// preconditions `next`/`prev` rely on for their linear scan.
    #[test]
    fn point_sets_are_well_formed(spans in spans(), markers in prop::collection::vec(0u64..2000, 0..5), level in 0u8..=16) {
        let tl = timeline(&spans, &markers);
        let jp = JumpPoints::build(&tl, None, Zoom::new(level), &JumpConfig::default(), &[]);
        let pts = jp.points();
        prop_assert!(pts.windows(2).all(|w| w[0] < w[1]));
        prop_assert!(pts.iter().all(|p| *p <= tl.duration()));
        prop_assert_eq!(pts.first().copied(), Some(Frame::ZERO));
    }

    /// Zooming in never removes a jump point.
    #[test]
    fn zooming_in_only_adds_points(spans in spans(), level in 0u8..16) {
        let tl = timeline(&spans, &[]);
        let cfg = JumpConfig::default();
        let out = JumpPoints::build(&tl, None, Zoom::new(level), &cfg, &[]);
        let inn = JumpPoints::build(&tl, None, Zoom::new(level + 1), &cfg, &[]);
        for p in out.points() {
            prop_assert!(inn.points().contains(p), "zooming in dropped {p}");
        }
    }

    /// A motion either lands inside the timeline or is rejected. It can never
    /// put the playhead somewhere that does not exist.
    #[test]
    fn motions_land_inside_the_timeline(spans in spans(), start in 0u64..2000, level in 0u8..=16, fwd in any::<bool>(), count in 0u32..8) {
        let mut tl = timeline(&spans, &[]);
        let v1 = track_id(&tl, "V1");
        let last = tl.duration().get().saturating_sub(1);
        tl.set_playhead_frame(Frame(start.min(last)));
        let jumps = JumpPoints::build(&tl, Some(v1), Zoom::new(level), &JumpConfig::default(), &[]);
        let ctx = MotionCtx::new(&tl, &jumps);
        let dir = if fwd { Direction::Forward } else { Direction::Backward };
        for m in [
            M::Frame(dir),
            M::JumpPoint(dir),
            M::ClipBoundary(dir),
            M::ClipEnd,
            M::TimelineStart,
            M::TimelineEnd,
            M::MatchingEdit,
        ] {
            if let Ok(r) = m.resolve(&ctx, count)
                && let Some(f) = r.frame()
            {
                prop_assert!(f <= tl.duration(), "{m:?} landed at {f} past the end");
            }
        }
    }
}
