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
            "                                                            \n",
            "                                                            \n",
            "                                                            \n",
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
    // The app placed it in the editing area, whose first row is the first
    // row the terminal draws; the terminal puts it there and nowhere else.
    let row = &drawn[panel.rect.row as usize];
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
    let body: String = drawn[1 + panel.rect.row as usize]
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

/// Regression: a which-key panel was cut off at the bottom, showing only
/// what fitted inside the *timeline* - so a project with three tracks could
/// never show more than three lines of it.
#[test]
fn a_panel_taller_than_the_track_count_still_fits_the_terminal() {
    use davimci_app::{
        App, PanelAnchor, PanelContent, PanelId, PanelLine, PanelOp, PanelRole, PanelSpan,
        PanelSpec, PanelStore, Surface,
    };
    let _ = PanelStore::default();

    let (width, height) = (60u16, 20u16);
    let surface: Surface = davimci_tui::render::surface(width, height, 0, 0);
    let mut app = App::new(davimci_cmd::Session::new(davimci_app::fixtures::timeline()));
    app.resize(surface);

    // Three tracks, ten lines of keys: the panel is taller than the lanes.
    let entries = 10;
    let id = PanelId(1);
    app.apply_panel(PanelOp::Open {
        id,
        spec: Box::new(PanelSpec {
            owner: "which-key".into(),
            title: Some("which-key".into()),
            anchor: PanelAnchor::BottomLeft,
            ..PanelSpec::default()
        }),
    });
    app.apply_panel(PanelOp::SetContent {
        id,
        content: PanelContent::Lines(
            (0..entries)
                .map(|i| PanelLine {
                    spans: vec![PanelSpan::new(format!("key {i}"), PanelRole::Key)],
                })
                .collect(),
        ),
    });

    let view = app.view();
    assert!(
        view.panels[0].rect.rows as usize >= entries + 2,
        "the panel was clamped to the tracks: {:?}",
        view.panels[0].rect
    );

    let drawn = rows(&view, width, height);
    let text = drawn.join("\n");
    for i in 0..entries {
        assert!(
            text.contains(&format!("key {i}")),
            "line {i} of the panel was cut off:\n{text}"
        );
    }
    // The status line still has its row, and nothing is ragged.
    assert!(
        drawn.last().unwrap().contains("NORMAL"),
        "the panel pushed the status line off:\n{text}"
    );
    for r in &drawn {
        assert_eq!(r.chars().count(), usize::from(width));
    }
}

/// Regression: on a terminal with no spare rows, opening `:` pushed the line
/// being typed off the bottom, because the app was still handing back a track
/// list sized for a screen without a command line.
#[test]
fn the_command_line_is_drawn_on_a_terminal_with_no_spare_rows() {
    use davimci_app::{App, NullHost};
    use davimci_tui::{Modifiers, TermKey};

    let (width, height) = (60u16, 5u16);
    let mut tui = Tui::new(width, height);
    let mut app = App::new(davimci_cmd::Session::new(fixtures::timeline()));
    app.resize(tui.surface());
    let mut host = NullHost;

    for c in [':', 'w'] {
        tui.push(TermEvent::Key(TermKey::Char(c), Modifiers::default()));
    }
    for event in tui.poll() {
        app.event(event, &mut host);
    }
    tui.render(&app.view()).expect("the tui draws");

    let drawn = tui.last_rows();
    assert!(
        drawn.len() <= usize::from(height),
        "the tui drew {} rows into {height}: {drawn:?}",
        drawn.len()
    );
    assert!(
        drawn.iter().any(|r| r.starts_with(":w")),
        "the command line was pushed off the bottom: {drawn:?}"
    );
}

/// The status line and the `:` line sit on the last rows of the terminal,
/// whatever the project's track count is.
#[test]
fn the_footer_is_anchored_to_the_bottom_of_the_terminal() {
    let (width, height) = (60u16, 20u16);
    let drawn = rows(&fixtures::normal(), width, height);
    assert_eq!(drawn.len(), usize::from(height));
    assert!(
        drawn[usize::from(height) - 1].starts_with("-- NORMAL"),
        "the status line is not on the last row: {drawn:?}"
    );
    assert!(
        drawn[4..usize::from(height) - 1]
            .iter()
            .all(|r| r.trim().is_empty()),
        "the rows between the tracks and the footer are not blank: {drawn:?}"
    );
}

/// The caret follows the text being typed, so a terminal shows where the next
/// character lands.
#[test]
fn the_caret_sits_in_the_command_line_while_it_is_typed() {
    use davimci_app::CommandLineView;

    let mut view = fixtures::normal();
    assert_eq!(davimci_tui::render::cursor(&view, 20), None);

    view.command_line = Some(CommandLineView {
        buffer: "write".into(),
        cursor: 3,
        completions: Vec::new(),
    });
    // One row for the `:` line: it is the last row, and the caret is past the
    // `:` and the three characters typed before it.
    assert_eq!(davimci_tui::render::cursor(&view, 20), Some((4, 19)));

    view.command_line = Some(CommandLineView {
        buffer: "w".into(),
        cursor: 1,
        completions: vec!["write".into()],
    });
    // A completion row is drawn under the `:` line, which moves up.
    assert_eq!(davimci_tui::render::cursor(&view, 20), Some((2, 18)));
}
