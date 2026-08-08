//! Terminal snapshots at fixed sizes, driven by the shared golden view
//! states, so a view-state regression fails in `davimci-app` *and* here.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{Entry, Frontend, MediaPicker, PickerIntent, SubtitleEdit, fixtures};
use davimci_tui::{GUTTER, TermEvent, Tui};

/// Render a view at a fixed size and hand back the plain rows.
fn rows(view: &davimci_app::ViewState, width: u16, height: u16) -> Vec<String> {
    let mut tui = Tui::new(width, height);
    tui.render(view).expect("the tui draws");
    tui.last_rows()
}

#[test]
fn every_row_is_exactly_as_wide_as_the_terminal() {
    for (name, view) in fixtures::all() {
        for (w, h) in [(80u16, 12u16), (40, 8), (120, 24)] {
            for row in rows(&view, w, h) {
                assert_eq!(
                    row.chars().count(),
                    usize::from(w),
                    "{name} at {w}x{h} drew a ragged row: {row:?}"
                );
            }
        }
    }
}

#[test]
fn the_normal_view_snapshots() {
    let drawn = rows(&fixtures::normal(), 60, 8).join("\n");
    assert_eq!(
        drawn,
        concat!(
            "time     │▼──────┼┬───────┬─────┼─┬┼─────┼──────────────────\n",
            ">V1      ││a██████b██████████████  █c█████                  \n",
            " A1      ││music█████████████████                           \n",
            " V2      ││  █over██                                        \n",
            "-- NORMAL (V1) --                                      0/500",
        ),
        "the terminal drew something the view state did not describe"
    );
}

#[test]
fn the_ruler_marks_the_playhead_and_the_clip_boundaries() {
    let drawn = rows(&fixtures::normal(), 60, 8);
    let ruler = &drawn[0];
    let cells: Vec<char> = ruler.chars().skip(usize::from(GUTTER)).collect();
    assert!(cells.contains(&'\u{253c}'), "no major tick on the ruler");
    assert_eq!(
        cells.iter().filter(|c| **c == '\u{25bc}').count(),
        1,
        "the playhead is drawn exactly once"
    );
}

#[test]
fn a_selection_is_drawn_on_every_selected_track() {
    let view = fixtures::visual_across_tracks();
    let mut tui = Tui::new(60, 10);
    let lines = tui.rows(&view);
    tui.render(&view).unwrap();
    // A selected clip is styled, not re-spelled, so the parity is in the
    // styles: two lanes carry reversed spans, one per selected track.
    let reversed = lines
        .iter()
        .filter(|l| {
            l.spans.iter().any(|s| {
                s.style
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            })
        })
        .count();
    assert_eq!(reversed, 2, "one selected lane per track in the block");
}

#[test]
fn only_the_covered_columns_of_a_clip_are_inverted() {
    let view = fixtures::visual_across_tracks();
    let (first, last) = view
        .selection
        .as_ref()
        .and_then(|s| s.columns)
        .expect("the selection is on screen");
    let track = view.tracks[0].id;
    let mut tui = Tui::new(60, 10);
    let lines = tui.rows(&view);
    tui.render(&view).unwrap();
    let row = view
        .tracks
        .iter()
        .position(|t| t.id == track)
        .expect("V1 is on screen");
    // Row 0 is the ruler, and every lane starts after the gutter.
    let line = &lines[row + 1];
    let mut column = 0u32;
    let mut cell = 0usize;
    let mut inverted = Vec::new();
    for span in &line.spans {
        for _ in span.content.chars() {
            if cell >= usize::from(GUTTER) {
                if span
                    .style
                    .add_modifier
                    .contains(ratatui::style::Modifier::REVERSED)
                {
                    inverted.push(column);
                }
                column += 1;
            }
            cell += 1;
        }
    }
    assert!(!inverted.is_empty(), "the selection is not drawn at all");
    assert_eq!(
        (
            inverted.iter().copied().min().unwrap_or(0),
            inverted.iter().copied().max().unwrap_or(0)
        ),
        (first, last),
        "the inversion is not exactly the selection's columns"
    );
}

#[test]
fn an_audio_lane_draws_its_envelope_inside_the_clip() {
    let drawn = rows(&fixtures::waveform(), 60, 8);
    let audio = drawn
        .iter()
        .find(|r| r.starts_with(" A1"))
        .expect("the audio lane is drawn");
    let blocks = "\u{2581}\u{2582}\u{2583}\u{2584}\u{2585}\u{2586}\u{2587}";
    assert!(
        audio.chars().any(|c| blocks.contains(c)),
        "the envelope is flat: {audio:?}"
    );
}

#[test]
fn the_command_line_takes_a_row_from_the_tracks() {
    let view = fixtures::normal();
    let mut tui = Tui::new(60, 8);
    tui.render(&view).unwrap();
    let before = tui.surface().rows;

    let mut open = fixtures::normal();
    open.command_line = Some(davimci_app::CommandLineView {
        buffer: "wri".into(),
        cursor: 3,
        completions: vec!["write".into()],
    });
    tui.render(&open).unwrap();
    assert_eq!(
        tui.surface().rows,
        before - 2,
        "the : line drew over a track"
    );
    let drawn = tui.last_rows();
    assert!(drawn[drawn.len() - 2].starts_with(":wri"));
    assert!(drawn[drawn.len() - 1].starts_with("write"));
}

#[test]
fn a_picker_takes_the_timeline_rows_and_gives_them_back() {
    let view = fixtures::normal();
    let mut tui = Tui::new(60, 8);
    tui.open_picker(MediaPicker::new(
        PickerIntent::Insert,
        vec![Entry::dir("/m/clips"), Entry::file("/m/bunny.mkv")],
    ));
    tui.render(&view).unwrap();
    let drawn = tui.last_rows();
    assert!(drawn[1].starts_with("insert media at the playhead"));
    assert!(drawn.iter().any(|r| r.contains("clips/")));
    assert!(drawn.iter().any(|r| r.contains("bunny.mkv")));
    assert!(
        !drawn.iter().any(|r| r.starts_with(">V1")),
        "the timeline is still drawn under the picker"
    );

    tui.push(TermEvent::Key(
        davimci_tui::TermKey::Escape,
        davimci_tui::Modifiers::default(),
    ));
    tui.poll();
    tui.render(&view).unwrap();
    assert!(tui.last_rows()[1].starts_with(">V1"));
}

#[test]
fn a_subtitle_edit_shows_the_buffer_being_typed() {
    let view = fixtures::normal();
    let mut tui = Tui::new(60, 8);
    tui.open_subtitle(SubtitleEdit::new(davimci_core::ClipId(1), "hello\nthere"));
    tui.render(&view).unwrap();
    let drawn = tui.last_rows();
    assert!(drawn[1].starts_with("subtitle text"));
    assert!(drawn[2].starts_with("hello"));
    assert!(drawn[3].starts_with("there"));
}

#[test]
fn a_message_and_a_pending_count_reach_the_status_line() {
    let mut view = fixtures::normal();
    view.pending = "3d".into();
    view.message = Some(davimci_app::Message::error("nothing to delete"));
    let drawn = rows(&view, 60, 8);
    let status = drawn.last().unwrap();
    assert!(status.contains("nothing to delete"));
    assert!(status.contains("3d "), "the pending count is not shown");
}

#[test]
fn a_plugin_panel_is_drawn_where_the_app_placed_it() {
    let view = davimci_app::fixtures::panel();
    let panel = &view.panels[0];
    let drawn = rows(&view, 60, 8);
    // The app placed it in track rows; the terminal draws it there and
    // nowhere else. Row 0 is the ruler.
    let row = &drawn[1 + panel.rect.row as usize];
    let at = usize::from(GUTTER) + panel.rect.column as usize;
    let box_row: String = row
        .chars()
        .skip(at)
        .take(panel.rect.columns as usize)
        .collect();
    assert!(
        box_row.starts_with('\u{250c}') && box_row.contains("which-key"),
        "the panel's frame and title are not at its placement: {box_row:?}"
    );
    let body: String = drawn[2 + panel.rect.row as usize]
        .chars()
        .skip(at)
        .take(panel.rect.columns as usize)
        .collect();
    assert!(
        body.contains("go to the start"),
        "the panel body is missing: {body:?}"
    );
    // And a panel changes no row's width.
    for r in &drawn {
        assert_eq!(r.chars().count(), 60, "a panel made a row ragged: {r:?}");
    }
}
