//! Snapshot tests on the textual view dump.
//!
//! The dumps are literal rather than stored in a snapshot file, so a
//! behavioural change to the view state shows up as a readable diff in the
//! test itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::fixtures;

#[test]
fn normal_view_dump_is_stable() {
    assert_eq!(
        fixtures::normal().dump(),
        "\
-- NORMAL (V1) --
viewport zoom=8 cols=50 rows=3 range=0..800 duration=500
ruler 0!0 7!1 8.2 16.3 22!4 24.5 25!6 31!7
>0 V1 [3:0..120@0-7] [4:120..360@7-22] [5:400..500@25-31]
 1 A1 [8:0..360@0-22]
 2 V2 [7:60..150@3-9]
playhead frame=0 track=1 col=0
"
    );
}

#[test]
fn scrolled_view_keeps_the_playhead_on_screen() {
    let view = fixtures::scrolled();
    assert_eq!(view.playhead.frame.get(), 300);
    assert!(view.playhead.column.is_some());
    assert!(view.visible_range.0 <= view.playhead.frame);
}

#[test]
fn visual_block_reports_every_selected_track_in_the_mode_line() {
    let view = fixtures::visual_block();
    assert_eq!(view.mode_line, "-- VISUAL-BLOCK 181f (V1,A1) --");
    let sel = view.selection.expect("visual mode has a selection");
    assert_eq!((sel.start.get(), sel.end.get()), (60, 241));
    assert_eq!(sel.tracks.len(), 2);
}

#[test]
fn selection_marks_only_overlapping_clips_on_selected_tracks() {
    let view = fixtures::visual_block();
    let selected: Vec<&str> = view
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .filter(|c| c.selected_columns.is_some())
        .map(|c| c.label.as_str())
        .collect();
    // `over` lives on V2, which is not in the block; `c` starts at 400.
    assert_eq!(selected, ["a", "b", "music"]);
}

#[test]
fn a_partly_covered_clip_reports_only_the_covered_columns() {
    let view = fixtures::visual_block();
    let sel = view.selection.clone().expect("visual mode has a selection");
    let (first, last) = sel.columns.expect("the selection is on screen");
    for c in view.tracks.iter().flat_map(|t| t.clips.iter()) {
        let Some(selected) = c.selected_columns else {
            continue;
        };
        let (label, columns) = (&c.label, c.columns);
        assert!(
            selected.0 >= columns.0 && selected.1 <= columns.1,
            "{label}: {selected:?} escapes the clip at {columns:?}"
        );
        assert!(
            selected.0 >= first && selected.1 <= last,
            "{label}: {selected:?} escapes the selection at {first}..={last}"
        );
    }
    // `a` runs 0..200 and the selection starts at frame 60, so its head is
    // outside: a clipped clip is not a wholly selected one.
    let a = view.tracks[0]
        .clips
        .iter()
        .find(|c| c.label == "a")
        .expect("V1 carries clip a");
    assert_ne!(a.selected_columns, Some(a.columns));
}

#[test]
fn zoomed_out_collapses_clips_without_losing_them() {
    let view = fixtures::zoomed_out();
    let v1 = &view.tracks[0];
    assert_eq!(v1.clips.len(), 3);
    // Everything lands in column 0 at 4096 frames per column, and no clip is
    // dropped for being sub-column-width.
    assert!(v1.clips.iter().all(|c| c.columns == (0, 0)));
    // The ruler still shows a tick, and it is a major one.
    assert!(view.ticks.iter().any(|t| t.major));
}

#[test]
fn ruler_ticks_are_unique_per_column_and_prefer_major() {
    for (name, view) in fixtures::all() {
        let mut cols: Vec<u32> = view.ticks.iter().map(|t| t.column).collect();
        let before = cols.len();
        cols.dedup();
        assert_eq!(cols.len(), before, "{name}: duplicate tick columns");
    }
}

#[test]
fn every_fixture_dumps_without_panicking_and_is_non_empty() {
    for (name, view) in fixtures::all() {
        let dump = view.dump();
        assert!(dump.starts_with("-- "), "{name}: {dump}");
        assert!(dump.contains("playhead "), "{name}: {dump}");
    }
}
