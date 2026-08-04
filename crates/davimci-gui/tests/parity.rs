//! One scripted session, two frontends, identical view states. The three-way
//! test that adds the TUI lives in `davimci-tui`, which is the crate that can
//! see all three.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, Frontend, NullHost, Surface};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_gui::{Gui, GuiEvent, Modifiers, RawKey};
use davimci_headless::HeadlessFrontend;

const SCRIPT: &str = "lljs";

fn session() -> Session {
    Session::new(fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")]),
        ("A1", &[(0, 300, "music")]),
    ]))
}

#[test]
fn the_gui_and_the_headless_frontend_agree_on_the_view() {
    // The GUI's surface is whatever its layout derives; the headless
    // frontend is told the same one, so the only variable left is the
    // frontend itself.
    let mut gui = Gui::new(800, 600);
    let surface: Surface = gui.surface();

    let mut gui_app = App::new(session());
    let mut host = NullHost;
    gui_app.resize(surface);
    for c in SCRIPT.chars() {
        gui.push(GuiEvent::Key(RawKey::Char(c), Modifiers::default()));
        for event in gui.poll() {
            gui_app.event(event, &mut host);
        }
    }
    gui.render(&gui_app.view()).expect("gui draws");

    let mut headless = HeadlessFrontend::script(surface, SCRIPT);
    let mut headless_app = App::new(session());
    headless_app
        .run(&mut headless, &mut NullHost)
        .expect("headless runs");

    assert_eq!(
        gui_app.view().dump(),
        headless_app.view().dump(),
        "the frontends diverged"
    );
    assert!(gui.last_draw().is_some_and(|d| !d.is_empty()));
}

#[test]
fn the_gui_translates_a_whole_session_without_losing_a_key() {
    let mut gui = Gui::new(640, 480);
    for c in SCRIPT.chars() {
        gui.push(GuiEvent::Key(RawKey::Char(c), Modifiers::default()));
    }
    let events = gui.poll();
    assert_eq!(events.len(), SCRIPT.len());
}
