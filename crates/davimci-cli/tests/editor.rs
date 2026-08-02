//! The assembled editor: keys in, commands run, backend projected, frames
//! presented - driven through the headless frontend so none of it needs a
//! window (plan.md Phase 9a/9b wiring).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, Event, Host, Surface};
use davimci_backend::{MockBackend, RenderBackend};
use davimci_cli::{Editor, TransportState, Workspace};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_core::{Fps, Resolution, Timeline};
use davimci_headless::HeadlessFrontend;
use davimci_keys::Key;
use davimci_present::{Host as PresentHost, Presenter};

fn timeline() -> Timeline {
    fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")]),
        ("A1", &[(0, 300, "music")]),
    ])
}

fn editor() -> (App, Editor) {
    let session = Session::new(timeline());
    let mut ws = Workspace::new(std::env::temp_dir()).without_autosave();
    ws.set_current_session(session.clone());
    let backend: Box<dyn RenderBackend> = Box::new(MockBackend::new(Resolution {
        width: 8,
        height: 4,
    }));
    let presenter = Presenter::new(
        PresentHost::Embedded,
        Resolution {
            width: 32,
            height: 16,
        },
        Fps::FPS_60,
    );
    let mut editor = Editor::new(ws, backend, presenter);
    let app = App::new(session);
    editor.prime(app.session());
    (app, editor)
}

fn feed(app: &mut App, editor: &mut Editor, keys: &str) {
    for k in Key::parse_str(keys) {
        app.key(k, editor);
    }
}

#[test]
fn startup_projects_the_timeline_and_shows_the_first_frame() {
    let (_app, editor) = editor();
    let p = editor.presentation().expect("a primed frame");
    assert_eq!(p.position, Some(davimci_core::Frame::ZERO));
}

#[test]
fn an_edit_reprojects_the_backend() {
    let (mut app, mut editor) = editor();
    // Step one frame in so the split lands inside a clip, then split.
    feed(&mut app, &mut editor, "<Right>s");
    assert!(
        app.session().timeline().tracks()[0].clips().len() > 3,
        "the split did not apply"
    );
    // The presenter followed the playhead to the new position.
    assert_eq!(
        editor.presentation().and_then(|p| p.position),
        Some(davimci_core::Frame(1))
    );
}

#[test]
fn a_motion_seeks_and_presents_without_touching_the_undo_log() {
    let (mut app, mut editor) = editor();
    let before = app.session().history().current();
    feed(&mut app, &mut editor, "l");
    let at = app.session().timeline().playhead().frame;
    assert!(at > davimci_core::Frame::ZERO);
    assert_eq!(editor.presentation().and_then(|p| p.position), Some(at));
    assert_eq!(
        app.session().history().current(),
        before,
        "a motion entered the undo log"
    );
}

#[test]
fn space_space_starts_playback_and_the_playhead_follows_the_clock() {
    let (mut app, mut editor) = editor();
    feed(&mut app, &mut editor, "  ");
    assert_eq!(editor.transport_state(), TransportState::Playing);
    for _ in 0..5 {
        app.event(Event::Tick, &mut editor);
    }
    assert!(
        app.session().timeline().playhead().frame > davimci_core::Frame::ZERO,
        "the playhead did not follow playback"
    );
    // And pausing stops the backend.
    feed(&mut app, &mut editor, "  ");
    assert_eq!(editor.transport_state(), TransportState::Stopped);
}

#[test]
fn shuttle_keys_step_the_playhead_and_k_stops() {
    let (mut app, mut editor) = editor();
    feed(&mut app, &mut editor, "L");
    assert_eq!(editor.transport_state(), TransportState::Shuttling(1));
    for _ in 0..3 {
        app.event(Event::Tick, &mut editor);
    }
    assert_eq!(
        app.session().timeline().playhead().frame,
        davimci_core::Frame(3)
    );
    feed(&mut app, &mut editor, "K");
    assert_eq!(editor.transport_state(), TransportState::Stopped);
    let held = app.session().timeline().playhead().frame;
    app.event(Event::Tick, &mut editor);
    assert_eq!(app.session().timeline().playhead().frame, held);
}

#[test]
fn playback_stops_by_itself_at_the_end_of_the_timeline() {
    let (mut app, mut editor) = editor();
    feed(&mut app, &mut editor, "  ");
    for _ in 0..1_000 {
        app.event(Event::Tick, &mut editor);
        if editor.transport_state() == TransportState::Stopped {
            break;
        }
    }
    assert_eq!(editor.transport_state(), TransportState::Stopped);
}

#[test]
fn a_colon_command_runs_against_the_session_the_user_can_see() {
    let (mut app, mut editor) = editor();
    feed(&mut app, &mut editor, "<Right>s");
    let clips = app.session().timeline().tracks()[0].clips().len();

    let dir = tempdir();
    let path = dir.join("proj.davimci");
    app.event(Event::Command(format!("w {}", path.display())), &mut editor);
    assert!(path.exists(), "the project was not written");

    // What landed on disk is the edited timeline, not a stale copy.
    let text = std::fs::read_to_string(&path).unwrap();
    let reopened = davimci_cmd::ProjectFile::from_json(&text)
        .unwrap()
        .into_session()
        .unwrap();
    assert_eq!(reopened.timeline().tracks()[0].clips().len(), clips);
}

#[test]
fn switching_buffers_hands_the_app_a_different_timeline() {
    let (mut app, mut editor) = editor();
    app.event(Event::Command("new".into()), &mut editor);
    let swapped = editor.take_session_swap().expect("a new timeline");
    assert!(swapped.timeline().tracks()[0].clips().is_empty());
    app.replace_session(swapped);
    assert!(app.session().timeline().tracks()[0].clips().is_empty());
    assert_eq!(app.mode(), davimci_keys::Mode::Normal);
}

#[test]
fn an_unknown_command_reports_a_sentence_and_keeps_editing() {
    let (mut app, mut editor) = editor();
    app.event(Event::Command("nope".into()), &mut editor);
    let msg = app.messages().current().expect("an error");
    assert_eq!(msg.severity, davimci_app::Severity::Error);
    feed(&mut app, &mut editor, "l");
    assert!(app.session().timeline().playhead().frame > davimci_core::Frame::ZERO);
}

#[test]
fn quitting_through_the_workspace_ends_the_loop() {
    let (mut app, mut editor) = editor();
    app.event(Event::Command("q!".into()), &mut editor);
    assert!(editor.wants_quit());
}

#[test]
fn a_scripted_session_runs_end_to_end_through_the_headless_frontend() {
    let (mut app, mut editor) = editor();
    let mut fe = HeadlessFrontend::script(
        Surface {
            columns: 60,
            rows: 4,
        },
        "ll<Right>s",
    );
    app.run(&mut fe, &mut editor).expect("the loop runs");
    assert!(fe.last_frame().unwrap().starts_with("-- NORMAL"));
    assert!(editor.presentation().is_some());
}

fn tempdir() -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("davimci-editor-{}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    base
}
