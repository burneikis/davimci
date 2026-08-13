//! One scripted session, three frontends, identical view states.
//!
//! This is the enforcer, not a formality: headless, GUI and TUI run the same
//! keys against the same timeline and must end on the same view. A failure
//! here is a frontend bug by construction - the core cannot tell the three
//! apart.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, Entry, Frontend, MediaPicker, NullHost, PickerIntent, Surface};
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
    // Both fill the terminal, because the status line is anchored to its last
    // row; the band's rows are blank until a frame has been composed, and the
    // written rows are the same ones in the same order.
    assert_eq!(preview_rows.len(), plain_rows.len());
    assert!(preview_rows[..4].iter().all(|r| r.trim().is_empty()));
    let written = |rows: &[String]| -> Vec<String> {
        rows.iter()
            .filter(|r| !r.trim().is_empty())
            .cloned()
            .collect()
    };
    assert_eq!(written(&preview_rows), written(&plain_rows));
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

/// One key in the alphabet both frontends bind, so a script can be written
/// once and replayed through either.
#[derive(Clone, Copy)]
enum Press {
    Char(char),
    Ctrl(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    Space,
    Left,
    Right,
    Up,
    Down,
}

fn typed(text: &str) -> Vec<Press> {
    text.chars().map(Press::Char).collect()
}

fn gui_press(press: Press) -> GuiEvent {
    let ctrl = GuiModifiers::ctrl();
    let none = GuiModifiers::default();
    let (key, mods) = match press {
        Press::Char(c) => (RawKey::Char(c), none),
        Press::Ctrl(c) => (RawKey::Char(c), ctrl),
        Press::Esc => (RawKey::Escape, none),
        Press::Enter => (RawKey::Enter, none),
        Press::Backspace => (RawKey::Backspace, none),
        Press::Tab => (RawKey::Tab, none),
        Press::Space => (RawKey::Space, none),
        Press::Left => (RawKey::Left, none),
        Press::Right => (RawKey::Right, none),
        Press::Up => (RawKey::Up, none),
        Press::Down => (RawKey::Down, none),
    };
    GuiEvent::Key(key, mods)
}

fn term_press(press: Press) -> TermEvent {
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    let none = Modifiers::default();
    let (key, mods) = match press {
        Press::Char(c) => (TermKey::Char(c), none),
        Press::Ctrl(c) => (TermKey::Char(c), ctrl),
        Press::Esc => (TermKey::Escape, none),
        Press::Enter => (TermKey::Enter, none),
        Press::Backspace => (TermKey::Backspace, none),
        Press::Tab => (TermKey::Tab, none),
        // A terminal has no Space key of its own; it sends the character.
        Press::Space => (TermKey::Char(' '), none),
        Press::Left => (TermKey::Left, none),
        Press::Right => (TermKey::Right, none),
        Press::Up => (TermKey::Up, none),
        Press::Down => (TermKey::Down, none),
    };
    TermEvent::Key(key, mods)
}

/// Replay a script through both frontends at the same surface, rendering
/// after every press so modal state stays in step, and report the two final
/// view dumps plus whether each still holds a picker.
fn both_frontends(script: &[Press], with_picker: bool) -> ((String, bool), (String, bool)) {
    let mut gui = Gui::new(800, 600);
    let surface: Surface = gui.surface();
    let mut tui = Tui::new(80, 12);

    let mut gui_app = App::new(session());
    let mut tui_app = App::new(session());
    gui_app.resize(surface);
    tui_app.resize(surface);
    let mut host = NullHost;

    if with_picker {
        gui.open_picker(picker());
        tui.open_picker(picker());
    }

    for press in script {
        gui.push(gui_press(*press));
        for event in gui.poll() {
            gui_app.event(event, &mut host);
        }
        gui.render(&gui_app.view()).expect("the gui draws");

        tui.push(term_press(*press));
        for event in tui.poll() {
            tui_app.event(event, &mut host);
        }
        tui.render(&tui_app.view()).expect("the tui draws");
    }

    (
        (gui_app.view().dump(), gui.picker().is_some()),
        (tui_app.view().dump(), tui.picker().is_some()),
    )
}

fn picker() -> MediaPicker {
    MediaPicker::new(
        PickerIntent::Insert,
        vec![Entry::file("/m/a.mkv"), Entry::file("/m/b.mkv")],
    )
}

/// Named keys and chords, not only letters: a frontend that named one of
/// these differently would edit differently.
#[test]
fn the_gui_and_the_tui_agree_on_named_keys_and_chords() {
    let mut script = typed("ll");
    script.extend([
        Press::Right,
        Press::Left,
        Press::Down,
        Press::Up,
        Press::Space,
        Press::Space,
        Press::Esc,
    ]);
    script.extend(typed("dd"));
    script.push(Press::Ctrl('r'));
    let (gui, tui) = both_frontends(&script, false);
    assert_eq!(gui.0, tui.0, "the GUI and the TUI diverged on named keys");
}

/// The `:` line is modal input, which headless cannot drive - so the two
/// interactive frontends are checked against each other.
#[test]
fn the_gui_and_the_tui_agree_on_a_typed_command_line() {
    let mut script = vec![Press::Char(':')];
    script.extend(typed("sett"));
    script.push(Press::Backspace);
    script.extend(typed(" nu"));
    script.push(Press::Enter);
    let (gui, tui) = both_frontends(&script, false);
    assert_eq!(gui.0, tui.0, "the command line diverged");

    let cancelled = vec![Press::Char(':'), Press::Char('w'), Press::Esc];
    let (gui, tui) = both_frontends(&cancelled, false);
    assert_eq!(gui.0, tui.0, "cancelling the command line diverged");
}

/// A modal owns plain keys in both frontends, and neither lets a chord be
/// swallowed by it.
#[test]
fn a_picker_swallows_the_same_keys_in_both_frontends() {
    let script = [Press::Char('a'), Press::Tab, Press::Down, Press::Backspace];
    let (gui, tui) = both_frontends(&script, true);
    assert_eq!(gui.0, tui.0, "the picker diverged");
    assert!(gui.1 && tui.1, "one frontend closed the picker early");

    let chord = [Press::Ctrl('r')];
    let (gui, tui) = both_frontends(&chord, true);
    assert_eq!(gui.0, tui.0, "a chord through an open picker diverged");
    assert!(
        gui.1 && tui.1,
        "a chord must reach the grammar without closing the picker"
    );
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
