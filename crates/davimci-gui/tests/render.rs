//! Rendering tests driven by the Phase 9a golden view states, so a
//! view-state regression fails in `davimci-app` *and* here (plan.md 9c).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::Frontend;
use davimci_app::fixtures;
use davimci_gui::paint::{Fill, Paint, TextRole, summarise};
use davimci_gui::{Chrome, Gui, Layout, Metrics, PickerIntent, VideoQuad, paint_view};

fn layout(width: u32, height: u32) -> Layout {
    Layout::compute(width, height, Metrics::default(), false, false)
}

#[test]
fn the_normal_view_paints_a_stable_draw_list() {
    let view = fixtures::normal();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    assert_eq!(
        summarise(&list),
        "Background=2 Clip=5 ClipLabel=5 Playhead=1 Ruler=1 RulerNumber=2 Status=1 StatusLine=1 \
TickMajor=5 TickMinor=3 TrackHeader=3 TrackLane=2 TrackLaneFocused=1 TrackName=3"
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
        picker: None,
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
                let l = Layout::compute(w, h, Metrics::default(), open, false);
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
    let l = Layout::compute(800, 600, Metrics::default(), true, false);
    assert!(
        paint_view(&view, &l, &Chrome::default())
            .rects(Fill::CommandLine)
            .is_empty()
    );
    view.command_line = Some(davimci_app::CommandLineView {
        buffer: "wq".into(),
        cursor: 2,
        completions: Vec::new(),
    });
    let list = paint_view(&view, &l, &Chrome::default());
    assert_eq!(list.rects(Fill::CommandLine).len(), 1);
    assert!(list.texts().contains(&":wq"));
    assert_eq!(
        list.rects(Fill::Caret).len(),
        1,
        "a line being typed needs a caret"
    );
}

/// The typed line, its caret and its suggestions are all painted from the
/// view state, so the user can see what they are typing (idea.md).
#[test]
fn completions_are_painted_on_their_own_row_above_the_line() {
    let mut view = fixtures::normal();
    view.command_line = Some(davimci_app::CommandLineView {
        buffer: "b".into(),
        cursor: 1,
        completions: vec!["b".into(), "bn".into(), "bp".into()],
    });
    let l = Layout::compute(800, 600, Metrics::default(), true, true);
    let row = l.completions.expect("a row for the suggestions");
    let list = paint_view(&view, &l, &Chrome::default());
    assert!(
        list.texts()
            .iter()
            .any(|t| t.contains("bn") && t.contains("bp")),
        "{:?}",
        list.texts()
    );
    assert!(
        row.y < l.command.expect("a command row").y,
        "suggestions sit above the line they complete"
    );
}

/// Regression: pressing `i` opened the picker but nothing painted it, so the
/// modal swallowed every key while the window looked frozen. A modal that is
/// open must be on screen.
#[test]
fn an_open_picker_is_painted() {
    let view = fixtures::normal();
    let mut gui = Gui::new(800, 600);

    // Nothing modal yet: no picker in the draw list.
    gui.render(&view).unwrap();
    let before = summarise(gui.last_draw().expect("a draw list"));
    assert!(
        !before.contains("ModalBackground"),
        "a picker was painted before one was opened:\n{before}"
    );

    gui.open_picker_at(PickerIntent::Insert, std::path::Path::new("."));
    gui.render(&view).unwrap();
    let after = summarise(gui.last_draw().expect("a draw list"));
    assert!(
        after.contains("ModalBackground"),
        "the open picker was never painted:\n{after}"
    );
    assert!(
        after.contains("ModalTitle"),
        "the picker was painted with no title:\n{after}"
    );
}

/// The title says what the chosen file will be used for, so `i`, `a` and `r`
/// are told apart once the picker is open.
#[test]
fn the_picker_title_names_the_intent() {
    let view = fixtures::normal();
    for (intent, want) in [
        (PickerIntent::Insert, "insert"),
        (PickerIntent::Append, "append"),
        (PickerIntent::Replace, "replace"),
    ] {
        let mut gui = Gui::new(800, 600);
        gui.open_picker_at(intent, std::path::Path::new("."));
        gui.render(&view).unwrap();
        let text: String = gui
            .last_draw()
            .expect("a draw list")
            .ops()
            .iter()
            .filter_map(|op| match op {
                Paint::Text { text, .. } => Some(text.clone()),
                Paint::Rect { .. } | Paint::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(text.contains(want), "expected {want:?} in:\n{text}");
    }
}

/// Spec §6.1: an analysed audio lane draws its envelope, and only that lane.
#[test]
fn an_analysed_audio_lane_paints_a_waveform() {
    let view = fixtures::waveform();
    let l = layout(800, 600);
    let list = paint_view(&view, &l, &Chrome::default());
    let bars = list.rects(Fill::Waveform);
    assert!(!bars.is_empty(), "the analysed lane painted no envelope");

    // Every bar sits inside the one lane that has audio.
    let row = view
        .tracks
        .iter()
        .position(|t| !t.waveform.is_empty())
        .expect("a lane has a waveform");
    let lane_y = l.lane_y(row);
    for bar in &bars {
        assert!(bar.width == 1, "an envelope column is one pixel wide");
        assert!(
            bar.y >= lane_y && bar.y + bar.height as i32 <= lane_y + l.metrics.row_height as i32,
            "an envelope bar escaped its lane: {bar:?}"
        );
    }
    // The fixture ramps from quiet to loud, so the last bar is the tallest.
    assert!(
        bars.last().expect("bars").height > bars[0].height,
        "the envelope ignored the levels it was given"
    );
}

/// Regression (idea.md): the envelope used to be painted over the clip
/// labels, so an analysed lane was a lane whose clips had no readable names.
/// Labels come last.
#[test]
fn clip_labels_are_painted_over_the_waveform() {
    let view = fixtures::waveform();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    let last_wave = list
        .ops()
        .iter()
        .rposition(|op| {
            matches!(
                op,
                Paint::Rect {
                    fill: Fill::Waveform,
                    ..
                }
            )
        })
        .expect("an envelope");
    // The last label belongs to the lane that has the envelope, since lanes
    // are painted top to bottom.
    let last_label = list
        .ops()
        .iter()
        .rposition(|op| {
            matches!(
                op,
                Paint::Text {
                    role: TextRole::ClipLabel,
                    ..
                }
            )
        })
        .expect("a clip label");
    assert!(
        last_label > last_wave,
        "labels are drawn under the envelope that hides them"
    );
}

/// A decoded thumbnail is drawn inside its clip, never wider than it
/// (idea.md).
#[test]
fn a_clip_thumbnail_is_drawn_inside_its_clip() {
    let mut view = fixtures::normal();
    let thumb = davimci_app::Thumbnail::new(4, 2, vec![255u8; 32], davimci_core::Frame(0));
    let clip = view.tracks[0].clips[0].clone();
    view.tracks[0].clips[0].thumbnail = Some(thumb);
    let l = layout(800, 600);
    let list = paint_view(&view, &l, &Chrome::default());
    let images = list.images();
    assert_eq!(images.len(), 1, "one clip has a picture");
    let (rect, id, _) = images[0];
    assert_eq!(id, clip.id);
    let (first, last) = clip.columns;
    assert!(rect.x >= l.tracks.x + first as i32);
    assert!(
        rect.x + rect.width as i32 <= l.tracks.x + last as i32 + 1,
        "the thumbnail spilled onto the next clip: {rect:?}"
    );
}

/// A lane with no analysis draws nothing rather than a flat line, so
/// "not analysed yet" cannot be mistaken for "silent".
#[test]
fn an_unanalysed_lane_paints_no_envelope() {
    let list = paint_view(&fixtures::normal(), &layout(800, 600), &Chrome::default());
    assert!(list.rects(Fill::Waveform).is_empty());
}
