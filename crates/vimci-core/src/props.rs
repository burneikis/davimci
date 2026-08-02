//! Property tests for the timeline model (plan.md Phase 1).
//!
//! The contract under test: whatever sequence of primitives runs, the
//! invariants hold, durations account exactly, and any rejected operation
//! leaves the timeline byte-identical.

#![cfg(test)]
#![allow(clippy::unwrap_used)]

use proptest::prelude::*;

use crate::clip::Clip;
use crate::error::{Classify, ErrorClass};
use crate::id::TrackId;
use crate::time::Frame;
use crate::timeline::Timeline;
use crate::trim::Edge;

/// One primitive, as a test-only script step.
#[derive(Debug, Clone)]
enum Op {
    Split(u64),
    RippleDelete(u64, u64),
    Lift(u64, u64),
    Insert(u64, u64),
    Overwrite(u64, u64),
    YankPaste(u64, u64, u64),
    TrimTail(usize, i64),
    TrimHead(usize, i64),
    Roll(u64, i64),
    Slide(usize, i64),
    MoveClip(usize, u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u64..400).prop_map(Op::Split),
        (0u64..400, 1u64..120).prop_map(|(s, l)| Op::RippleDelete(s, s + l)),
        (0u64..400, 1u64..120).prop_map(|(s, l)| Op::Lift(s, s + l)),
        (0u64..400, 1u64..80).prop_map(|(at, l)| Op::Insert(at, l)),
        (0u64..400, 1u64..80).prop_map(|(at, l)| Op::Overwrite(at, l)),
        (0u64..400, 1u64..80, 0u64..400).prop_map(|(s, l, at)| Op::YankPaste(s, s + l, at)),
        (0usize..6, -60i64..60).prop_map(|(i, d)| Op::TrimTail(i, d)),
        (0usize..6, -60i64..60).prop_map(|(i, d)| Op::TrimHead(i, d)),
        (0u64..400, -60i64..60).prop_map(|(c, d)| Op::Roll(c, d)),
        (0usize..6, -60i64..60).prop_map(|(i, d)| Op::Slide(i, d)),
        (0usize..6, 0u64..400).prop_map(|(i, s)| Op::MoveClip(i, s)),
    ]
}

fn base() -> Timeline {
    crate::testing::media_fixture(&[
        (0, 100, 50, 400),
        (100, 80, 50, 400),
        (180, 120, 50, 400),
        (340, 60, 50, 400),
    ])
}

/// Run one op. Returns whether it mutated (i.e. was accepted).
fn run(tl: &mut Timeline, v1: TrackId, op: &Op) -> bool {
    let ids = crate::testing::clip_ids(tl, "V1");
    let pick = |i: usize| ids.get(i % ids.len().max(1)).copied();
    let res: Result<(), crate::CoreError> = match *op {
        Op::Split(f) => tl.split_at(v1, Frame(f)).map(|_| ()),
        Op::RippleDelete(a, b) => tl.ripple_delete_range(v1, Frame(a), Frame(b)).map(|_| ()),
        Op::Lift(a, b) => tl.lift_range(v1, Frame(a), Frame(b)).map(|_| ()),
        Op::Insert(at, len) => {
            let id = tl.new_clip_id();
            let c = Clip::generated(id, "n", Frame::ZERO, Frame(len));
            tl.insert_clip(v1, c, Frame(at)).map(|_| ())
        }
        Op::Overwrite(at, len) => {
            let id = tl.new_clip_id();
            let c = Clip::generated(id, "n", Frame::ZERO, Frame(len));
            tl.overwrite_clip(v1, c, Frame(at)).map(|_| ())
        }
        Op::YankPaste(a, b, at) => match tl.yank_range(v1, Frame(a), Frame(b)) {
            Ok(reg) if !reg.is_empty() => tl.paste(v1, Frame(at), &reg, true).map(|_| ()),
            Ok(_) => Err(crate::CoreError::EmptyRegister),
            Err(e) => Err(e),
        },
        Op::TrimTail(i, d) => match pick(i) {
            Some(c) => tl.ripple_trim(v1, c, Edge::Tail, d),
            None => Ok(()),
        },
        Op::TrimHead(i, d) => match pick(i) {
            Some(c) => tl.ripple_trim(v1, c, Edge::Head, d),
            None => Ok(()),
        },
        Op::Roll(cut, d) => tl.roll(v1, Frame(cut), d),
        Op::Slide(i, d) => match pick(i) {
            Some(c) => tl.slide(v1, c, d),
            None => Ok(()),
        },
        Op::MoveClip(i, s) => match pick(i) {
            Some(c) => tl.move_clip(v1, c, Frame(s)),
            None => Ok(()),
        },
    };
    match res {
        Ok(()) => true,
        Err(e) => {
            // Nothing in this crate may produce a corruption-class error from
            // ordinary user input.
            assert_eq!(e.class(), ErrorClass::User, "{e}");
            assert!(!e.user_message().is_empty());
            false
        }
    }
}

proptest! {
    /// Invariants survive any sequence of primitives.
    #[test]
    fn invariants_hold_under_random_edits(ops in prop::collection::vec(op(), 1..40)) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        for o in &ops {
            run(&mut tl, v1, o);
            tl.check_invariants().map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
    }

    /// A rejected operation leaves the timeline byte-identical (Phase 0.1).
    #[test]
    fn rejected_operations_do_not_mutate(ops in prop::collection::vec(op(), 1..40)) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        for o in &ops {
            let before = tl.tracks().to_vec();
            if !run(&mut tl, v1, o) {
                prop_assert_eq!(tl.tracks(), before.as_slice(), "mutated on rejected {:?}", o);
            }
        }
    }

    /// Ripple delete removes exactly the requested span of content.
    #[test]
    fn ripple_delete_shortens_by_the_overlap(start in 0u64..380, len in 1u64..100) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        let end = start + len;
        let before = tl.duration().get();
        let covered: u64 = tl
            .track(v1)
            .map(|t| t.clips().iter()
                .map(|c| c.end().get().min(end).saturating_sub(c.start.get().max(start)))
                .sum())
            .unwrap_or(0);
        tl.ripple_delete_range(v1, Frame(start), Frame(end)).map_err(|e| TestCaseError::fail(e.to_string()))?;
        // Removing content shortens the track by the covered frames plus any
        // gap inside the range that later clips slid across.
        let after = tl.duration().get();
        prop_assert!(after <= before.saturating_sub(covered), "{after} vs {before} - {covered}");
    }

    /// Lifting never changes where later clips sit.
    #[test]
    fn lift_preserves_later_positions(start in 0u64..380, len in 1u64..100) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        let end = start + len;
        let later: Vec<u64> = tl.track(v1).map(|t| t.clips().iter()
            .filter(|c| c.start.get() >= end)
            .map(|c| c.start.get()).collect()).unwrap_or_default();
        tl.lift_range(v1, Frame(start), Frame(end)).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let after: Vec<u64> = tl.track(v1).map(|t| t.clips().iter()
            .filter(|c| c.start.get() >= end)
            .map(|c| c.start.get()).collect()).unwrap_or_default();
        // Lift may split a straddling clip, adding a start exactly at `end`,
        // but it must never move anything that was already past the range.
        for s in later {
            prop_assert!(after.contains(&s), "clip at {} moved on lift", s);
        }
    }

    /// Splitting is content-preserving: same total covered frames, one more
    /// clip, and the two halves are exactly adjacent.
    #[test]
    fn split_preserves_content(frame in 1u64..400) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        let covered = |t: &Timeline| -> u64 {
            t.track(v1).map(|t| t.clips().iter().map(|c| c.duration.get()).sum()).unwrap_or(0)
        };
        let before = covered(&tl);
        let n = tl.track(v1).map(|t| t.clips().len()).unwrap_or(0);
        if tl.split_at(v1, Frame(frame)).is_ok() {
            prop_assert_eq!(covered(&tl), before);
            prop_assert_eq!(tl.track(v1).map(|t| t.clips().len()), Some(n + 1));
        }
        tl.check_invariants().map_err(|e| TestCaseError::fail(e.to_string()))?;
    }

    /// Yank then paste at the end reproduces the yanked content exactly.
    #[test]
    fn yank_paste_round_trips(start in 0u64..300, len in 1u64..100) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        let reg = match tl.yank_range(v1, Frame(start), Frame(start + len)) {
            Ok(r) if !r.is_empty() => r,
            _ => return Ok(()),
        };
        let at = Frame(tl.duration().get() + 1000);
        let ids = tl.paste(v1, at, &reg, true).map_err(|e| TestCaseError::fail(e.to_string()))?;
        for (id, original) in ids.iter().zip(reg.clips.iter()) {
            let (_, c) = tl.find_clip(*id).ok_or_else(|| TestCaseError::fail("pasted clip missing"))?;
            prop_assert_eq!(c.duration, original.duration);
            prop_assert_eq!(c.source_in, original.source_in);
            prop_assert_eq!(c.start.get(), at.get() + original.start.get());
        }
    }

    /// Roll never changes the total duration of the track.
    #[test]
    fn roll_is_duration_neutral(delta in -60i64..60) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        let before = tl.duration();
        if tl.roll(v1, Frame(100), delta).is_ok() {
            prop_assert_eq!(tl.duration(), before);
        }
    }

    /// Slip changes neither position nor duration, only the source window.
    #[test]
    fn slip_moves_nothing_on_the_timeline(delta in -60i64..60) {
        let mut tl = base();
        let v1 = crate::testing::track_id(&tl, "V1");
        let before = tl.dump();
        let Some(c) = crate::testing::clip_ids(&tl, "V1").first().copied() else {
            return Ok(());
        };
        let _ = tl.slip(v1, c, delta);
        prop_assert_eq!(tl.dump(), before);
    }
}
