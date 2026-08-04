//! Table-driven motion tests.

use davimci_core::testing::{clip_ids, fixture, track_id};
use davimci_core::{Frame, Marker, Timeline, TrackId};

use crate::error::MotionError;
use crate::jump::{JumpConfig, JumpPoints, Zoom};
use crate::motion::{BuiltinMotion as M, Motion, MotionCtx};
use crate::object::{Object, TextObject as O};
use crate::predicate::{Predicate, testing::StubIndex};
use crate::target::{Direction::Backward, Direction::Forward, Resolved, TimeRange};

/// `V1: [a 0-100][b 100-250]<gap 50>[c 300-350]`, `A1: [x 0-100]`, empty `A2`,
/// markers at 120 and 300.
fn tl() -> Timeline {
    let mut tl = fixture(&[
        ("V1", &[(0, 100, "a"), (100, 150, "b"), (300, 50, "c")]),
        ("A1", &[(0, 100, "x")]),
        ("A2", &[]),
    ]);
    tl.markers.push(Marker {
        frame: Frame(120),
        label: "one".into(),
    });
    tl.markers.push(Marker {
        frame: Frame(300),
        label: "two".into(),
    });
    tl
}

fn place(tl: &mut Timeline, track: &str, frame: u64) {
    let id = track_id(tl, track);
    let _ = tl.focus_track(id);
    tl.set_playhead_frame(Frame(frame));
}

/// Resolve a motion at `(track, frame)` and report the landing frame.
fn land(track: &str, frame: u64, m: &M, count: u32) -> Result<u64, MotionError> {
    let mut tl = tl();
    place(&mut tl, track, frame);
    let jumps = JumpPoints::build(
        &tl,
        Some(tl.playhead().track),
        Zoom::OUT,
        &JumpConfig::default(),
        &[],
    );
    let ctx = MotionCtx::new(&tl, &jumps);
    m.resolve(&ctx, count).map(|r| match r {
        Resolved::Position(p) => p.frame.get(),
        Resolved::Range(r, _) => r.start.get(),
        Resolved::Pending => u64::MAX,
    })
}

#[test]
fn landing_positions() {
    // (track, start frame, motion, count, expected landing)
    let cases: &[(&str, u64, M, u32, u64)] = &[
        // Arrow keys: exactly one frame, at any zoom.
        ("V1", 10, M::Frame(Forward), 1, 11),
        ("V1", 10, M::Frame(Backward), 1, 9),
        ("V1", 10, M::Frame(Forward), 7, 17),
        // Clamped at both ends rather than failing.
        ("V1", 0, M::Frame(Backward), 5, 0),
        ("V1", 349, M::Frame(Forward), 5, 349),
        // Jump points, zoomed out: clip bounds and markers.
        ("V1", 0, M::JumpPoint(Forward), 1, 100),
        ("V1", 0, M::JumpPoint(Forward), 2, 120),
        ("V1", 0, M::JumpPoint(Forward), 3, 250),
        ("V1", 0, M::JumpPoint(Forward), 99, 350),
        ("V1", 350, M::JumpPoint(Backward), 1, 300),
        // Clip boundaries.
        ("V1", 0, M::ClipBoundary(Forward), 1, 100),
        ("V1", 100, M::ClipBoundary(Backward), 1, 0),
        ("V1", 120, M::ClipBoundary(Backward), 1, 100),
        ("V1", 120, M::ClipBoundary(Forward), 2, 300),
        ("V1", 349, M::ClipBoundary(Forward), 1, 350),
        // `e`: last frame of the clip, then onward.
        ("V1", 0, M::ClipEnd, 1, 99),
        ("V1", 99, M::ClipEnd, 1, 249),
        ("V1", 0, M::ClipEnd, 3, 349),
        // In the gap, `e` finds the next clip's end.
        ("V1", 260, M::ClipEnd, 1, 349),
        // Timeline ends.
        ("V1", 120, M::TimelineStart, 1, 0),
        ("V1", 120, M::TimelineEnd, 1, 349),
        // Markers.
        ("V1", 0, M::Marker(Forward), 1, 120),
        ("V1", 0, M::Marker(Forward), 2, 300),
        ("V1", 300, M::Marker(Backward), 1, 120),
        // `%` toggles between the ends of the clip under the playhead.
        ("V1", 100, M::MatchingEdit, 1, 249),
        ("V1", 249, M::MatchingEdit, 1, 100),
        ("V1", 180, M::MatchingEdit, 1, 100),
        // An empty track has no boundaries but still has the timeline ends.
        ("A2", 40, M::TimelineEnd, 1, 349),
    ];

    for (track, start, m, count, want) in cases {
        let got = land(track, *start, m, *count);
        assert_eq!(
            got,
            Ok(*want),
            "{m:?} with count {count} from {track}:{start}"
        );
    }
}

#[test]
fn motions_that_cannot_land_are_rejected() {
    let cases: &[(&str, u64, M, MotionError)] = &[
        ("V1", 350, M::JumpPoint(Forward), MotionError::NoJumpPoint),
        ("V1", 0, M::JumpPoint(Backward), MotionError::NoJumpPoint),
        ("A2", 0, M::ClipBoundary(Forward), MotionError::NoBoundary),
        ("V1", 350, M::ClipBoundary(Forward), MotionError::NoBoundary),
        ("V1", 349, M::ClipEnd, MotionError::NoBoundary),
        ("V1", 300, M::Marker(Forward), MotionError::NoMarker),
        (
            "V1",
            260,
            M::MatchingEdit,
            MotionError::NoMatchingEdit { frame: 260 },
        ),
        ("V1", 0, M::Mark('z'), MotionError::NoSuchMark('z')),
    ];
    for (track, start, m, want) in cases {
        assert_eq!(land(track, *start, m, 1), Err(want.clone()), "{m:?}");
    }
}

#[test]
fn marks_carry_their_track() {
    let mut tl = tl();
    place(&mut tl, "V1", 0);
    let a1 = track_id(&tl, "A1");
    tl.marks.insert(
        'a',
        davimci_core::Mark {
            frame: Frame(222),
            track: Some(a1),
        },
    );
    let jumps = JumpPoints::default();
    let ctx = MotionCtx::new(&tl, &jumps);
    let got = M::Mark('a').resolve(&ctx, 1);
    assert!(matches!(
        got,
        Ok(Resolved::Position(p)) if p.frame == Frame(222) && p.track == a1
    ));
}

#[test]
fn track_focus_clamps_but_cycling_wraps() {
    let mut tl = tl();
    let names = ["V1", "A1", "A2"];
    let ids: Vec<TrackId> = names.iter().map(|n| track_id(&tl, n)).collect();
    let jumps = JumpPoints::default();

    let focus = |tl: &Timeline, m: &M, count: u32| {
        let ctx = MotionCtx::new(tl, &jumps);
        m.resolve(&ctx, count).map(|r| match r {
            Resolved::Position(p) => p.track,
            _ => TrackId(0),
        })
    };

    place(&mut tl, "V1", 0);
    assert_eq!(focus(&tl, &M::TrackStep(Forward), 1), Ok(ids[1]));
    assert_eq!(focus(&tl, &M::TrackStep(Forward), 9), Ok(ids[2]));
    assert_eq!(
        focus(&tl, &M::TrackStep(Backward), 1),
        Err(MotionError::NoTrackThere)
    );
    assert_eq!(focus(&tl, &M::TrackCycle(Backward), 1), Ok(ids[2]));
    assert_eq!(focus(&tl, &M::TrackCycle(Forward), 3), Ok(ids[0]));

    place(&mut tl, "A2", 0);
    assert_eq!(
        focus(&tl, &M::TrackStep(Forward), 1),
        Err(MotionError::NoTrackThere)
    );
    assert_eq!(focus(&tl, &M::TrackCycle(Forward), 1), Ok(ids[0]));
}

#[test]
fn jump_points_get_denser_as_zoom_increases() {
    let tl = tl();
    let v1 = track_id(&tl, "V1");
    let cfg = JumpConfig::default();
    let mut tl2 = tl.clone();
    place(&mut tl2, "V1", 0);

    let land_at = |zoom: Zoom| {
        let jumps = JumpPoints::build(&tl2, Some(v1), zoom, &cfg, &[]);
        let ctx = MotionCtx::new(&tl2, &jumps);
        M::JumpPoint(Forward)
            .resolve(&ctx, 1)
            .ok()
            .and_then(|r| r.frame())
    };
    assert_eq!(land_at(Zoom::OUT), Some(Frame(100)));
    // Spacing is 8 columns wide at every level, so it only gets finer than a
    // 250-frame timeline well into the zoom range.
    assert_eq!(land_at(Zoom::new(2)), Some(Frame(100)));
    assert_eq!(land_at(Zoom::new(10)), Some(Frame(32)));
    assert_eq!(land_at(Zoom::MAX), Some(Frame(1)));
}

// text objects

fn resolve_object(tl: &Timeline, o: &O) -> Result<(TimeRange, Vec<TrackId>), MotionError> {
    let jumps = JumpPoints::default();
    let ctx = MotionCtx::new(tl, &jumps);
    match o.resolve(&ctx)? {
        Resolved::Range(r, s) => Ok((r, s.tracks().to_vec())),
        other => Err(MotionError::NoSegment).inspect_err(|_| drop(other)),
    }
}

/// The object decides the scope, not the verb.
#[test]
fn object_scope_matrix() {
    // Ungrouped: every object stays on the focused track.
    let mut plain = tl();
    place(&mut plain, "V1", 120);
    let v1 = track_id(&plain, "V1");
    for o in [O::InnerClip, O::AClip, O::InnerTrack, O::ATrack] {
        let got = resolve_object(&plain, &o);
        assert_eq!(
            got,
            Ok((TimeRange::new(Frame(100), Frame(250)), vec![v1])),
            "{o:?} on an unlinked clip"
        );
    }

    // Linked video+audio: only `at` follows the group.
    let mut linked = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "x")])]);
    let (v, a) = (track_id(&linked, "V1"), track_id(&linked, "A1"));
    let ids = [clip_ids(&linked, "V1"), clip_ids(&linked, "A1")].concat();
    assert!(linked.link(&ids).is_ok());
    place(&mut linked, "V1", 50);

    let full = TimeRange::new(Frame(0), Frame(100));
    for o in [O::InnerClip, O::AClip, O::InnerTrack] {
        assert_eq!(resolve_object(&linked, &o), Ok((full, vec![v])), "{o:?}");
    }
    assert_eq!(resolve_object(&linked, &O::ATrack), Ok((full, vec![v, a])));
}

#[test]
fn a_segment_object_needs_a_visual_selection() {
    let mut tl = tl();
    place(&mut tl, "V1", 120);
    let v1 = track_id(&tl, "V1");
    assert_eq!(
        resolve_object(&tl, &O::InnerSegment(None)),
        Err(MotionError::NoSegment)
    );
    let seg = TimeRange::new(Frame(110), Frame(130));
    assert_eq!(
        resolve_object(&tl, &O::InnerSegment(Some(seg))),
        Ok((seg, vec![v1]))
    );
}

#[test]
fn objects_need_a_clip_under_the_playhead() {
    let mut tl = tl();
    place(&mut tl, "V1", 260);
    assert_eq!(
        resolve_object(&tl, &O::InnerClip),
        Err(MotionError::NoClipUnderPlayhead { track: "V1".into() })
    );
}

// predicate motions

#[test]
fn predicate_motions_report_pending_until_analysis_lands() {
    let mut tl = tl();
    place(&mut tl, "A1", 0);
    let a1 = track_id(&tl, "A1");
    let p = Predicate::AudioPeak {
        track: a1,
        threshold_db: -2.0,
    };
    let jumps = JumpPoints::default();

    // No index at all: Pending, never a guess.
    let ctx = MotionCtx::new(&tl, &jumps);
    assert!(
        M::Predicate(p.clone(), Forward)
            .resolve(&ctx, 1)
            .is_ok_and(|r| r.is_pending())
    );

    // Analysis in flight: still Pending.
    let running = StubIndex {
        hits: vec![(a1, Frame(40))],
        pending: true,
    };
    let ctx = MotionCtx::new(&tl, &jumps).with_analysis(&running);
    assert!(
        M::Predicate(p.clone(), Forward)
            .resolve(&ctx, 1)
            .is_ok_and(|r| r.is_pending())
    );

    // Complete: exact hits, counts chain, and no match is an error.
    let done = StubIndex {
        hits: vec![(a1, Frame(40)), (a1, Frame(90))],
        pending: false,
    };
    let ctx = MotionCtx::new(&tl, &jumps).with_analysis(&done);
    assert_eq!(
        M::Predicate(p.clone(), Forward)
            .resolve(&ctx, 1)
            .ok()
            .and_then(|r| r.frame()),
        Some(Frame(40))
    );
    assert_eq!(
        M::Predicate(p.clone(), Forward)
            .resolve(&ctx, 2)
            .ok()
            .and_then(|r| r.frame()),
        Some(Frame(90))
    );
    place(&mut tl, "A1", 90);
    let ctx = MotionCtx::new(&tl, &jumps).with_analysis(&done);
    assert_eq!(
        M::Predicate(p, Forward).resolve(&ctx, 1),
        Err(MotionError::NoPredicateMatch)
    );
}

#[test]
fn a_predicate_motion_moves_focus_to_the_track_it_searched() {
    let mut tl = tl();
    place(&mut tl, "V1", 0);
    let a1 = track_id(&tl, "A1");
    let idx = StubIndex {
        hits: vec![(a1, Frame(70))],
        pending: false,
    };
    let jumps = JumpPoints::default();
    let ctx = MotionCtx::new(&tl, &jumps).with_analysis(&idx);
    let got = M::Predicate(
        Predicate::Silence {
            track: a1,
            min_duration_ms: 500,
            threshold_db: -40.0,
        },
        Forward,
    )
    .resolve(&ctx, 1);
    assert!(matches!(
        got,
        Ok(Resolved::Position(p)) if p.frame == Frame(70) && p.track == a1
    ));
}
