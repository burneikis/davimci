//! Rendering tests driven by the Phase 9a golden view states, so a
//! view-state regression fails in `davimci-app` *and* here.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the assertions are integer pixel arithmetic on window-sized values"
)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::Frontend;
use davimci_app::fixtures;
use davimci_gui::paint::{Fill, Paint, TextRole, summarise};
use davimci_gui::{Chrome, Gui, Layout, Metrics, Numbers, PickerIntent, VideoQuad, paint_view};

fn layout(width: u32, height: u32) -> Layout {
    Layout::compute(width, height, Metrics::default(), false, false)
}

/// Chrome asking the ruler for numbers, which are off by default.
fn numbered(numbers: Numbers) -> Chrome {
    Chrome {
        numbers,
        ..Chrome::default()
    }
}

#[test]
fn the_normal_view_paints_a_stable_draw_list() {
    let view = fixtures::normal();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    assert_eq!(
        summarise(&list),
        "Background=2 Clip=5 ClipLabel=5 Cursor=1 Playhead=1 Ruler=1 Status=1 StatusLine=1 \
TickMajor=5 TickMinor=3 TrackHeader=2 TrackHeaderFocused=1 TrackLane=2 TrackLaneFocused=1 \
TrackName=3"
    );
}

/// The playhead spans every lane, so on its own it never says which track an
/// edit would land on. The cursor is that answer, and it belongs to exactly
/// one lane.
#[test]
fn the_cursor_marks_the_focused_lane_and_only_that_lane() {
    let view = fixtures::normal();
    let l = layout(800, 600);
    let list = paint_view(&view, &l, &Chrome::default());
    let cursors = list.rects(Fill::Cursor);
    assert_eq!(cursors.len(), 1, "one cursor, on one track");
    let row = view
        .tracks
        .iter()
        .position(|t| t.focused)
        .expect("a track has focus");
    assert_eq!(cursors[0].y, l.lane_y(row));
    assert_eq!(cursors[0].height, l.metrics.row_height);
    assert_eq!(list.rects(Fill::TrackHeaderFocused).len(), 1);
}

#[test]
fn a_selection_paints_one_band_per_selected_track() {
    let view = fixtures::visual_across_tracks();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    let bands = list.rects(Fill::Selection);
    assert_eq!(bands.len(), 2, "one band per track in the block");
    assert!(bands.iter().all(|b| b.width > 0));
    // The tint stops at the selection: a clip that runs past the band is
    // never painted selected end to end.
    let tints = list.rects(Fill::ClipSelected);
    assert!(!tints.is_empty());
    let band = bands[0];
    let right = band.x + i32::try_from(band.width).unwrap_or(0);
    for tint in &tints {
        let tint_right = tint.x + i32::try_from(tint.width).unwrap_or(0);
        assert!(
            tint.x >= band.x && tint_right <= right,
            "a clip tint at {tint:?} escapes the selection at {band:?}"
        );
    }
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
        ..Chrome::default()
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
    let view = fixtures::visual_across_tracks();
    let list = paint_view(&view, &layout(800, 600), &Chrome::default());
    assert!(
        list.texts()
            .iter()
            .any(|t| t.starts_with("-- VISUAL 181f (V1,A1) --")),
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
/// view state, so the user can see what they are typing.
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
    let command = l.command.expect("a command row");
    // Vim's order, bottom-up: the `:` line last, the mode line above it, the
    // suggestions above that. The terminal frontend draws the same order.
    assert!(
        row.y < l.status.y && l.status.y < command.y,
        "chrome order is suggestions, status, command: {row:?} {:?} {command:?}",
        l.status
    );
    assert_eq!(
        command.y + command.height as i32,
        600,
        "the : line is not on the last row of the window"
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
                Paint::Rect { .. } | Paint::Image { .. } | Paint::Picture { .. } => None,
            })
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(text.contains(want), "expected {want:?} in:\n{text}");
    }
}

/// An analysed audio lane draws its envelope, and only that lane.
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

/// Regression: the envelope used to be painted over the clip
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

/// Relative numbers belong to every jump point, not only to clip boundaries:
/// the number is the count that lands there, and `3l` is as useful mid-clip
/// as at a cut.
#[test]
fn relative_numbers_are_painted_for_subdivision_ticks_too() {
    let view = fixtures::normal();
    let l = layout(800, 600);
    let list = paint_view(&view, &l, &numbered(Numbers::Relative));
    let numbered: Vec<i32> = list
        .ops()
        .iter()
        .filter_map(|op| match op {
            Paint::Text {
                rect,
                role: TextRole::RulerNumber,
                ..
            } => Some(rect.x - l.ruler.x),
            _ => None,
        })
        .collect();
    let minor: Vec<i32> = view
        .ticks
        .iter()
        .filter(|t| !t.major)
        .map(|t| t.column as i32)
        .collect();
    assert!(
        numbered
            .iter()
            .any(|x| minor.iter().any(|c| (*x - c).abs() <= 2)),
        "only clip boundaries were numbered: numbers at {numbered:?}, subdivisions at {minor:?}"
    );
}

/// The three number modes, through the one decision `davimci-app` makes for
/// both frontends: nothing at all, the frame, or the count a motion needs.
#[test]
fn the_ruler_numbers_what_the_setting_asks_for_and_nothing_otherwise() {
    let view = fixtures::normal();
    let l = layout(800, 600);
    let numbers = |chrome: &Chrome| -> Vec<String> {
        paint_view(&view, &l, chrome)
            .ops()
            .iter()
            .filter_map(|op| match op {
                Paint::Text {
                    text,
                    role: TextRole::RulerNumber,
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect()
    };
    assert!(numbers(&numbered(Numbers::Off)).is_empty());

    let relative = numbers(&numbered(Numbers::Relative));
    assert!(!relative.is_empty());
    // A count, so the playhead's own jump point reads zero.
    assert!(relative.contains(&"0".to_string()), "{relative:?}");

    let absolute = numbers(&numbered(Numbers::Absolute));
    let frames: Vec<String> = view
        .ticks
        .iter()
        .map(|t| t.frame.get().to_string())
        .collect();
    assert!(!absolute.is_empty());
    assert!(
        absolute.iter().all(|n| frames.contains(n)),
        "{absolute:?} are not frames of {frames:?}"
    );
}

/// A decoded thumbnail is repeated across the whole clip, like a filmstrip,
/// and never spills past the clip's own edge.
#[test]
fn a_clip_thumbnail_tiles_across_the_clip_and_stays_inside_it() {
    let mut view = fixtures::normal();
    // A tall, narrow picture, so a tile is a few pixels wide at a normal
    // lane height and the clip holds several of them.
    let thumb = davimci_app::Thumbnail::new(2, 8, vec![255u8; 2 * 8 * 4], davimci_core::Frame(0));
    let clip = view.tracks[0].clips[1].clone();
    // Two sample points across the clip, as the app would place them.
    let (first_col, last_col) = clip.columns;
    view.tracks[0].clips[1].thumbnails = vec![
        (first_col, thumb.clone()),
        (first_col + (last_col - first_col) / 2, thumb),
    ];
    let win = layout(800, 600);
    let list = paint_view(&view, &win, &Chrome::default());
    let images = list.images();
    assert!(
        images.len() > 1,
        "one stamp is not a filmstrip: {} tiles",
        images.len()
    );
    assert!(
        images.iter().all(|(_, id, _)| *id == clip.id),
        "a tile belongs to a clip that is not the one it pictures"
    );
    let (first_col, last_col) = clip.columns;
    let left = win.tracks.x + first_col as i32;
    let right = win.tracks.x + last_col as i32 + 1;
    for (rect, _, _) in &images {
        assert!(rect.x >= left, "a tile started left of its clip: {rect:?}");
        assert!(
            rect.x + rect.width as i32 <= right,
            "a tile spilled onto the next clip: {rect:?}"
        );
    }
    // Each picture is drawn at the column the app placed it at.
    let want: Vec<i32> = view.tracks[0].clips[1]
        .thumbnails
        .iter()
        .map(|(c, _)| win.tracks.x + *c as i32)
        .collect();
    let got: Vec<i32> = images.iter().map(|(r, _, _)| r.x).collect();
    assert_eq!(got, want, "a picture drifted from its sample point");
}

/// A lane with no analysis draws nothing rather than a flat line, so
/// "not analysed yet" cannot be mistaken for "silent".
#[test]
fn an_unanalysed_lane_paints_no_envelope() {
    let list = paint_view(&fixtures::normal(), &layout(800, 600), &Chrome::default());
    assert!(list.rects(Fill::Waveform).is_empty());
}

#[test]
fn a_plugin_panel_is_painted_over_the_timeline() {
    let view = davimci_app::fixtures::panel();
    let mut gui = Gui::new(800, 600);
    gui.render(&view).unwrap();
    let list = gui.last_draw().expect("a draw list");
    let text: Vec<&str> = list.texts();
    assert!(
        text.contains(&"which-key") && text.iter().any(|t| t.trim() == "go to the start"),
        "the panel's text was not painted: {text:?}"
    );
    // Its background is drawn, and as a modal - so the video texture cannot
    // cover it.
    assert!(
        !list
            .rects(davimci_gui::paint::Fill::PanelBackground)
            .is_empty(),
        "the panel has no body"
    );
    assert!(
        list.ops()
            .iter()
            .filter(|op| matches!(
                op,
                davimci_gui::paint::Paint::Text {
                    role: davimci_gui::paint::TextRole::Panel(_),
                    ..
                }
            ))
            .all(davimci_gui::paint::Paint::is_modal),
        "panel text is not drawn as an overlay"
    );
}

/// Regression: the which-key panel drew each key half cut off.
///
/// A shell insets text by `TEXT_PADDING` and clips it to the rectangle it was
/// given, so a span box sized to its glyphs alone loses its tail - and a
/// one-glyph key span loses half of its only glyph.
#[test]
fn a_panel_span_has_room_for_its_glyphs_and_the_shells_inset() {
    let view = davimci_app::fixtures::panel();
    let mut gui = Gui::new(800, 600);
    gui.render(&view).unwrap();
    let metrics = davimci_gui::layout::Metrics::default();
    for op in gui.last_draw().expect("a draw list").ops() {
        let davimci_gui::paint::Paint::Text {
            rect,
            role: davimci_gui::paint::TextRole::Panel(_),
            text,
        } = op
        else {
            continue;
        };
        let glyphs = u32::try_from(text.chars().count()).unwrap() * metrics.char_width;
        assert!(
            rect.width >= glyphs + davimci_gui::layout::TEXT_PADDING,
            "{text:?} is drawn in {rect:?}, which clips {} px of it",
            glyphs + davimci_gui::layout::TEXT_PADDING - rect.width
        );
    }
}
