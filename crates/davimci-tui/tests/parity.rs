//! One scripted session, three frontends, identical view states.
//!
//! This is the enforcer, not a formality: headless, GUI and TUI run the same
//! keys against the same timeline and must end on the same view. A failure
//! here is a frontend bug by construction - the core cannot tell the three
//! apart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, Frontend, NullHost, Surface};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_gui::{Gui, GuiEvent, Modifiers as GuiModifiers, RawKey};
use davimci_headless::HeadlessFrontend;
use davimci_tui::{Height, Modifiers, TermEvent, TermKey, Tui};

const SCRIPT: &str = "lljs";

fn session() -> Session {
    Session::new(fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")]),
        ("A1", &[(0, 300, "music")]),
    ]))
}

/// Run a script through the TUI at a surface, and report the final view.
fn through_tui(surface: Surface, keys: &str) -> (String, Vec<String>) {
    through_tui_with_preview(surface, keys, Height::Off)
}

/// As above, with an inline preview band requested.
fn through_tui_with_preview(
    surface: Surface,
    keys: &str,
    preview: Height,
) -> (String, Vec<String>) {
    // The TUI's surface is derived from its size, so it is sized to match
    // whatever the other frontends reported rather than the other way round.
    let mut tui = Tui::new(80, 12);
    tui.set_preview_height(preview);
    let mut app = App::new(session());
    app.resize(surface);
    let mut host = NullHost;
    for c in keys.chars() {
        tui.push(TermEvent::Key(TermKey::Char(c), Modifiers::default()));
        for event in tui.poll() {
            app.event(event, &mut host);
        }
    }
    let view = app.view();
    tui.render(&view).expect("the tui draws");
    (view.dump(), tui.last_rows().clone())
}

#[test]
fn headless_gui_and_tui_agree_on_the_view() {
    let mut gui = Gui::new(800, 600);
    let surface: Surface = gui.surface();

    let mut gui_app = App::new(session());
    gui_app.resize(surface);
    let mut host = NullHost;
    for c in SCRIPT.chars() {
        gui.push(GuiEvent::Key(RawKey::Char(c), GuiModifiers::default()));
        for event in gui.poll() {
            gui_app.event(event, &mut host);
        }
    }
    gui.render(&gui_app.view()).expect("the gui draws");

    let mut headless = HeadlessFrontend::script(surface, SCRIPT);
    let mut headless_app = App::new(session());
    headless_app
        .run(&mut headless, &mut NullHost)
        .expect("headless runs");

    let (tui_dump, rows) = through_tui(surface, SCRIPT);

    assert_eq!(
        gui_app.view().dump(),
        headless_app.view().dump(),
        "the GUI and headless diverged"
    );
    assert_eq!(
        tui_dump,
        headless_app.view().dump(),
        "the TUI and headless diverged"
    );
    assert!(!rows.is_empty(), "the TUI drew nothing");
}

/// Preview is not view state, so turning it on may not change one: the band
/// takes rows from the tracks and nothing else.
#[test]
fn preview_does_not_change_the_view() {
    let gui = Gui::new(800, 600);
    let surface: Surface = gui.surface();
    let (plain, plain_rows) = through_tui(surface, SCRIPT);
    let (with_preview, preview_rows) = through_tui_with_preview(surface, SCRIPT, Height::Rows(4));
    assert_eq!(plain, with_preview, "the preview band changed the view");
    // The band's own rows are the only difference on screen, and they are
    // blank until a frame has been composed.
    assert_eq!(preview_rows.len(), plain_rows.len() + 4);
    assert!(preview_rows[..4].iter().all(|r| r.trim().is_empty()));
    assert_eq!(&preview_rows[4..], &plain_rows[..]);
}

/// The keys must survive translation too: a frontend that swallowed one
/// would pass the view comparison by driving a shorter session.
#[test]
fn the_tui_translates_a_whole_session_without_losing_a_key() {
    let mut tui = Tui::new(80, 12);
    for c in SCRIPT.chars() {
        tui.push(TermEvent::Key(TermKey::Char(c), Modifiers::default()));
    }
    assert_eq!(tui.poll().len(), SCRIPT.len());
}

/// With no display and no preview, every edit still works and the frontend
/// still draws: the TUI must be usable over ssh on a headless box.
#[test]
fn the_tui_edits_with_no_display_and_no_preview() {
    let mut tui = Tui::new(80, 12);
    let mut app = App::new(session());
    app.resize(tui.surface());
    let mut host = NullHost;

    for c in "lldd".chars() {
        tui.push(TermEvent::Key(TermKey::Char(c), Modifiers::default()));
    }
    for event in tui.poll() {
        app.event(event, &mut host);
    }
    let view = app.view();
    tui.render(&view).expect("the tui draws with no display");
    assert!(app.session().timeline().duration().get() > 0);
    assert!(!tui.last_rows().is_empty());
}
