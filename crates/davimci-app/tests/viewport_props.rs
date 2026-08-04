//! Viewport invariants under arbitrary motion/zoom sequences.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::Viewport;
use davimci_core::Frame;
use davimci_motion::Zoom;
use proptest::prelude::*;

#[derive(Debug, Clone, Copy)]
enum Op {
    Seek(u64),
    ZoomIn,
    ZoomOut,
    Scroll(i64),
    Resize(u32, usize),
    FollowTrack(usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u64..5_000).prop_map(Op::Seek),
        Just(Op::ZoomIn),
        Just(Op::ZoomOut),
        (-50i64..50).prop_map(Op::Scroll),
        (0u32..300, 0usize..10).prop_map(|(c, r)| Op::Resize(c, r)),
        (0usize..8).prop_map(Op::FollowTrack),
    ]
}

proptest! {
    #[test]
    fn playhead_stays_visible_and_viewport_stays_in_bounds(ops in prop::collection::vec(op(), 1..60)) {
        let duration = Frame(5_000);
        let tracks = 8;
        let mut vp = Viewport::new(80, 4);
        let mut playhead = Frame::ZERO;

        for o in ops {
            match o {
                Op::Seek(f) => {
                    playhead = Frame(f.min(duration.get()));
                    vp.follow_playhead(playhead, duration);
                }
                Op::ZoomIn => vp.zoom_in(playhead, duration),
                Op::ZoomOut => vp.zoom_out(playhead, duration),
                Op::Scroll(d) => {
                    vp.scroll_columns(d, playhead, duration);
                    vp.follow_playhead(playhead, duration);
                }
                Op::Resize(c, r) => {
                    vp.resize(c, r);
                    vp.follow_playhead(playhead, duration);
                }
                Op::FollowTrack(i) => vp.follow_track(i.min(tracks - 1), tracks),
            }

            prop_assert!(vp.contains(playhead), "playhead {playhead:?} outside {vp:?}");
            prop_assert!(vp.start() <= duration);
            let (top, bottom) = vp.visible_tracks();
            prop_assert!(top < tracks.max(1));
            prop_assert!(bottom > top);
        }
    }

    #[test]
    fn zoom_keeps_the_playhead_in_its_column(level in 0u8..16, ph in 0u64..4_000) {
        let duration = Frame(5_000);
        let playhead = Frame(ph);
        let mut vp = Viewport::new(64, 3);
        vp.set_zoom(Zoom::new(level), playhead, duration);
        vp.follow_playhead(playhead, duration);
        let before = vp.column_of(playhead);
        vp.zoom_in(playhead, duration);
        prop_assert_eq!(vp.column_of(playhead), before);
    }

    #[test]
    fn column_of_and_frame_at_column_agree(level in 0u8..16, col in 0u32..63) {
        let mut vp = Viewport::new(64, 3);
        vp.set_zoom(Zoom::new(level), Frame(1_000), Frame(100_000));
        let f = vp.frame_at_column(col);
        prop_assert_eq!(vp.column_of(f), Some(col));
    }
}
