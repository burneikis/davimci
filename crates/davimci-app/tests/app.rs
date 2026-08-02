//! App event-loop behaviour with an inline fake frontend (plan.md 9a).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, AppError, Event, Frontend, Host, NullHost, Response, Severity, Surface};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_core::{Frame, Timeline};
use davimci_keys::Key;

fn timeline() -> Timeline {
    fixture(&[
        ("V1", &[(0, 100, "a"), (100, 100, "b"), (200, 100, "c")]),
        ("A1", &[(0, 300, "music")]),
    ])
}

#[derive(Debug, Default)]
struct Recorder {
    frames: Vec<String>,
    events: Vec<Event>,
}

impl Frontend for Recorder {
    fn poll(&mut self) -> Vec<Event> {
        if self.events.is_empty() {
            vec![Event::Quit]
        } else {
            std::mem::take(&mut self.events)
        }
    }

    fn surface(&self) -> Surface {
        Surface {
            columns: 30,
            rows: 1,
        }
    }

    fn render(&mut self, view: &davimci_app::ViewState) -> Result<(), AppError> {
        self.frames.push(view.dump());
        Ok(())
    }
}

#[derive(Debug, Default)]
struct TestHost {
    commands: Vec<String>,
    quit: bool,
}

impl Host for TestHost {
    fn command(&mut self, line: &str, _s: &mut Session) -> Result<Option<String>, AppError> {
        self.commands.push(line.to_string());
        if line == "q" {
            self.quit = true;
        }
        Ok(Some(format!("ran :{line}")))
    }

    fn wants_quit(&self) -> bool {
        self.quit
    }
}

#[test]
fn an_edit_reports_its_description_in_the_status_line() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = NullHost;
    // A split at frame 0 lands on a clip boundary and is rejected, so step
    // one frame in first - `<Right>` is the fixed one-frame motion (spec §11).
    for k in Key::parse_str("<Right>s") {
        app.key(k, &mut host);
    }
    let msg = app.messages().current().expect("split reports itself");
    assert_eq!(msg.severity, Severity::Info);
    assert!(!msg.text.is_empty());
}

#[test]
fn an_unbound_key_warns_rather_than_failing() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = NullHost;
    app.key(Key::Char('Z'), &mut host);
    let msg = app.messages().current().expect("a warning");
    assert_eq!(msg.severity, Severity::Warning);
}

#[test]
fn motion_scrolls_the_viewport_to_follow_the_playhead() {
    let mut app = App::new(Session::new(timeline()));
    app.set_zoom(davimci_motion::Zoom::MAX);
    app.resize(Surface {
        columns: 20,
        rows: 2,
    });
    let mut host = NullHost;
    for k in Key::parse_str("lll") {
        app.key(k, &mut host);
    }
    let view = app.view();
    assert!(view.playhead.frame > Frame::ZERO);
    assert!(app.viewport().contains(view.playhead.frame));
}

#[test]
fn track_focus_change_scrolls_vertically() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 20,
        rows: 1,
    });
    let mut host = NullHost;
    app.key(Key::Char('j'), &mut host);
    let view = app.view();
    assert_eq!(view.tracks.len(), 1);
    assert!(view.tracks[0].focused);
    assert_eq!(view.tracks[0].name, "A1");
}

#[test]
fn colon_opens_the_command_line_and_the_host_runs_the_line() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TestHost::default();
    assert_eq!(
        app.key(Key::Char(':'), &mut host),
        Response::OpenCommandLine
    );
    assert!(app.view().command_line.is_some());
    let r = app.event(Event::Command("w".into()), &mut host);
    assert_eq!(r, Response::Continue);
    assert_eq!(host.commands, ["w"]);
    assert!(app.view().command_line.is_none());
    assert_eq!(
        app.messages().current().map(|m| m.text.as_str()),
        Some("ran :w")
    );
}

#[test]
fn a_host_that_wants_to_quit_ends_the_loop() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TestHost::default();
    assert_eq!(
        app.event(Event::Command("q".into()), &mut host),
        Response::Quit
    );
    assert!(app.wants_quit());
}

#[test]
fn an_unhandled_command_is_an_error_message_not_a_crash() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = NullHost;
    app.event(Event::Command("nope".into()), &mut host);
    let msg = app.messages().current().expect("an error");
    assert_eq!(msg.severity, Severity::Error);
    assert!(msg.text.ends_with('.'), "not a sentence: {}", msg.text);
}

#[test]
fn the_loop_renders_and_stops_when_the_frontend_quits() {
    let mut app = App::new(Session::new(timeline()));
    let mut fe = Recorder {
        events: vec![Event::Key(Key::Char('l'))],
        ..Recorder::default()
    };
    let mut host = NullHost;
    app.run(&mut fe, &mut host).expect("loop runs");
    assert!(!fe.frames.is_empty());
}

#[test]
fn resize_is_taken_from_the_frontend_before_the_first_render() {
    let mut app = App::new(Session::new(timeline()));
    let mut fe = Recorder::default();
    let mut host = NullHost;
    app.run(&mut fe, &mut host).expect("loop runs");
    assert_eq!(app.viewport().columns(), 30);
    assert_eq!(app.viewport().rows(), 1);
}

#[test]
fn zoom_keys_drive_the_viewport() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = NullHost;
    let start = app.viewport().zoom();

    for k in Key::parse_str("zi") {
        app.key(k, &mut host);
    }
    assert_eq!(app.viewport().zoom(), start.zoom_in());

    for k in Key::parse_str("zozo") {
        app.key(k, &mut host);
    }
    assert_eq!(app.viewport().zoom(), start.zoom_out());

    // `z0` returns to the default level from wherever zooming left off.
    for k in Key::parse_str("z0") {
        app.key(k, &mut host);
    }
    assert_eq!(app.viewport().zoom(), davimci_motion::Zoom::default());
}

/// A host whose import appends one 600-frame clip, so the app can be observed
/// fitting the viewport to it.
#[derive(Debug, Default)]
struct ImportHost;

impl Host for ImportHost {
    fn import_media(
        &mut self,
        _path: &std::path::Path,
        _intent: davimci_keys::MediaIntent,
        session: &mut Session,
    ) -> Result<Option<String>, AppError> {
        let track = session
            .timeline()
            .tracks()
            .first()
            .map(|t| t.id)
            .expect("fixture timelines have a track");
        let id = davimci_core::ClipId(9_001);
        let clip = davimci_core::Clip::generated(id, "imported", Frame(0), Frame(600));
        let at = session.timeline().duration();
        session
            .exec(&davimci_cmd::EditCommand::Insert {
                track,
                at,
                clip,
                new_id: None,
            })
            .map_err(|e| AppError::UnhandledCommand(e.to_string()))?;
        Ok(None)
    }
}

#[test]
fn first_import_fits_the_clip_in_the_viewport_width() {
    let mut app = App::new(Session::new(fixture(&[("V1", &[])])));
    app.resize(Surface {
        columns: 30,
        rows: 4,
    });
    let mut host = ImportHost;
    app.key(Key::parse_str("a")[0], &mut host);
    app.event(Event::MediaChosen("clip.mp4".into()), &mut host);

    let vp = app.viewport();
    assert_eq!(vp.start(), Frame::ZERO);
    assert!(vp.span() >= Frame(600), "clip does not fit: {vp:?}");
    assert!(vp.span() < Frame(2 * 600), "fit left the clip tiny: {vp:?}");
}

#[test]
fn a_later_import_leaves_the_viewport_alone() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 30,
        rows: 4,
    });
    let before = app.viewport();
    let mut host = ImportHost;
    app.key(Key::parse_str("a")[0], &mut host);
    app.event(Event::MediaChosen("clip.mp4".into()), &mut host);
    assert_eq!(app.viewport().zoom(), before.zoom());
}
