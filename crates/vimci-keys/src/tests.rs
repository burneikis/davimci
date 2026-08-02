//! End-to-end headless tests: feed keys, assert the resulting timeline
//! (plan.md Phase 4 testing).

use vimci_cmd::Session;
use vimci_core::testing::fixture;

use crate::engine::{Engine, Outcome};
use crate::key::Key;

fn scene() -> (Engine, Session) {
    (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 300, "a")])])),
    )
}

fn feed(engine: &mut Engine, session: &mut Session, s: &str) -> Vec<Outcome> {
    Key::parse_str(s)
        .into_iter()
        .map(|k| engine.feed(k, session))
        .collect()
}

#[test]
fn split_at_an_interior_frame_via_keys() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "50<Right>s");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), "V1:[a 0-50][a 50-300]\nA1: -\n");
}

#[test]
fn x_ripple_deletes_the_clip_under_the_playhead() {
    let (mut e, mut s) = scene();
    let out = feed(&mut e, &mut s, "x");
    assert!(matches!(out[0], Outcome::Applied(_)), "{out:?}");
    // Ripple delete closes the gap; deleting the only clip leaves nothing.
    assert_eq!(s.timeline().dump(), "V1: -\nA1: -\n");
}

#[test]
fn dw_ripple_deletes_to_the_next_clip_boundary() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    let out = feed(&mut e, &mut s, "dw");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    // Ripple delete of `a` shifts `b` left to close the gap.
    assert_eq!(s.timeline().dump(), "V1:[b 0-100]\nA1: -\n");
}

#[test]
fn undo_redo_and_repeat_round_trip_through_keys() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    let start = s.timeline().dump();
    feed(&mut e, &mut s, "x");
    let after_x = s.timeline().dump();
    assert_ne!(start, after_x);
    let out = feed(&mut e, &mut s, "u");
    assert!(matches!(out[0], Outcome::Applied(_)));
    assert_eq!(s.timeline().dump(), start);
    feed(&mut e, &mut s, "<C-r>");
    assert_eq!(s.timeline().dump(), after_x);
}

#[test]
fn yank_and_paste_round_trip_through_keys() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    let out = feed(&mut e, &mut s, "yy");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "p");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_ne!(s.timeline().dump(), before);
}

#[test]
fn a_macro_records_and_replays_an_edit() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[(
            "V1",
            &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")],
        )])),
    );
    feed(&mut e, &mut s, "qax"); // record: ripple-delete the clip at 0
    let out = feed(&mut e, &mut s, "q"); // stop
    assert!(matches!(out[0], Outcome::MacroStopped('a')), "{out:?}");
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "@a");
    assert!(matches!(out.last(), Some(Outcome::Replayed(_))), "{out:?}");
    assert_ne!(
        s.timeline().dump(),
        before,
        "the macro must have deleted again"
    );
}

#[test]
fn esc_from_a_pending_sequence_never_touches_the_timeline() {
    let (mut e, mut s) = scene();
    let before = s.timeline().dump();
    let out = feed(&mut e, &mut s, "3d<Esc>");
    assert!(matches!(out.last(), Some(Outcome::Mode(_))), "{out:?}");
    assert_eq!(s.timeline().dump(), before);
}

#[test]
fn visual_mode_delete_acts_on_the_selection_and_exits_visual() {
    let (mut e, mut s) = (
        Engine::new(),
        Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])])),
    );
    feed(&mut e, &mut s, "v");
    assert_eq!(e.mode(), crate::mode::Mode::Visual);
    feed(&mut e, &mut s, "<Right>");
    let out = feed(&mut e, &mut s, "d");
    assert!(matches!(out.last(), Some(Outcome::Applied(_))), "{out:?}");
    assert_eq!(e.mode(), crate::mode::Mode::Normal);
}
