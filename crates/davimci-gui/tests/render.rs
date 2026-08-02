//! Rendering tests driven by the Phase 9a golden view states, so a
//! view-state regression fails in `davimci-app` *and* here (plan.md 9c).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::fixtures;
use davimci_gui::paint::{Fill, summarise};
use davimci_gui::{Chrome, Layout, Metrics, VideoQuad, paint_view};

fn layout(width: u32, height: u32) -> Layout {
    Layout::compute(width, height, Metrics::default(), false)
}

#[test]
fn the_normal_view_paints_a_stable_draw_list() {
    let view = fixtures::normal();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    assert_eq!(
        summarise(&list),
        "Background=2 Clip=5 ClipLabel=5 Playhead=1 Ruler=1 Status=1 StatusLine=1 \
TickMajor=5 TickMinor=27 TrackHeader=3 TrackLane=2 TrackLaneFocused=1 TrackName=3"
    );
}

#[test]
fn a_selection_paints_one_band_per_selected_track() {
    let view = fixtures::visual_block();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    let bands = list.rects(Fill::Selection);
    assert_eq!(bands.len(), 2, "one band per track in the block");
    assert!(bands.iter().all(|b| b.width > 0));
    assert!(!list.rects(Fill::ClipSelected).is_empty());
}

#[test]
fn the_playhead_spans_the_ruler_and_every_lane() {
    let view = fixtures::scrolled();
    let l = layout(800, 600);
    let list = paint_view(&view, &l, &Chrome::default());
    let ph = list.rects(Fill::Playhead);
    assert_eq!(ph.len(), 1);
    assert_eq!(ph[0].y, l.ruler.y);
    assert_eq!(ph[0].height, l.ruler.height + l.tracks.height);
    assert_eq!(ph[0].width, 1);
}

#[test]
fn zoomed_out_clips_still_paint_at_least_one_pixel_wide() {
    let view = fixtures::zoomed_out();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    for rect in list.rects(Fill::Clip) {
        assert!(rect.width >= 1, "a clip vanished: {rect:?}");
    }
}

#[test]
fn the_video_quad_is_placed_inside_the_video_pane() {
    let view = fixtures::normal();
    let l = layout(800, 600);
    let chrome = Chrome {
        video: Some(VideoQuad {
            x: 10,
            y: 4,
            width: 100,
            height: 50,
            timecode: Some("00:00:00:00"),
        }),
        command_cursor: 0,
    };
    let list = paint_view(&view, &l, &chrome);
    let video = list.rects(Fill::Video);
    assert_eq!(video.len(), 1);
    assert_eq!(video[0].x, l.video.x + 10);
    assert_eq!(video[0].y, l.video.y + 4);
    assert!(list.texts().contains(&"00:00:00:00"));
}

#[test]
fn every_golden_view_paints_at_extreme_sizes_without_panicking() {
    let sizes = [(1, 1), (1, 600), (800, 1), (4000, 3000), (0, 0)];
    for (name, view) in fixtures::all() {
        for (w, h) in sizes {
            for open in [false, true] {
                let l = Layout::compute(w, h, Metrics::default(), open);
                let list = paint_view(&view, &l, &Chrome::default());
                assert!(!list.is_empty(), "{name} at {w}x{h}");
                assert!(l.surface().columns >= 1);
                assert!(l.surface().rows >= 1);
            }
        }
    }
}

#[test]
fn the_status_line_carries_the_mode_line() {
    let view = fixtures::visual_block();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    assert!(
        list.texts()
            .iter()
            .any(|t| t.starts_with("-- VISUAL-BLOCK (V1,A1) --")),
        "{:?}",
        list.texts()
    );
}

#[test]
fn an_open_command_line_is_painted_only_when_the_view_has_one() {
    let mut view = fixtures::normal();
    let l = Layout::compute(800, 600, Metrics::default(), true);
    assert!(
        paint_view(&view, &l, &Chrome::default())
            .rects(Fill::CommandLine)
            .is_empty()
    );
    view.command_line = Some("wq".into());
    let list = paint_view(&view, &l, &Chrome::default());
    assert_eq!(list.rects(Fill::CommandLine).len(), 1);
    assert!(list.texts().contains(&":wq"));
}
