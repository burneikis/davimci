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

/// An editor whose exports run to completion only when polled, so a test can
/// observe an export *while* it is running.
fn editor_with_manual_render() -> (App, Editor) {
    let session = Session::new(timeline());
    let mut ws = Workspace::new(std::env::temp_dir()).without_autosave();
    ws.set_current_session(session.clone());
    let mut mock = MockBackend::new(Resolution {
        width: 8,
        height: 4,
    });
    mock.manual_render = true;
    let presenter = Presenter::new(
        PresentHost::Embedded,
        Resolution {
            width: 32,
            height: 16,
        },
        Fps::FPS_60,
    );
    let mut editor = Editor::new(ws, Box::new(mock), presenter);
    let app = App::new(session);
    editor.prime(app.session());
    (app, editor)
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

/// Regression: the composed frame is sized to the surface it was composed
/// for, and nothing recomposed it when the video pane resized - so the
/// picture kept its startup size and sat in the corner of the pane.
#[test]
fn resizing_the_video_pane_recomposes_the_frame_at_the_new_size() {
    let (app, mut editor) = editor();
    let start = editor.presentation().expect("a primed frame").surface;
    let bigger = Resolution {
        width: start.width * 2,
        height: start.height * 2,
    };
    editor.presenter_mut().resize(bigger);
    editor.refresh_preview(app.session());
    let after = editor.presentation().expect("a recomposed frame");
    assert_eq!(after.surface, bigger, "the frame kept its old surface");
    assert_eq!(
        after.pixels.len(),
        (bigger.width as usize) * (bigger.height as usize) * 4
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

// ---- export (Phase 8b) -------------------------------------------------

/// `:` commands the workspace cannot answer, because only the editor holds a
/// render backend.
#[test]
fn an_export_command_starts_a_render_and_reports_a_job() {
    let (mut app, mut editor) = editor_with_manual_render();
    let out = std::env::temp_dir().join("davimci-export-test.mkv");
    let line = format!(":export {}", out.display());
    app.event(Event::Command(line), &mut editor);

    assert!(editor.exporter().is_running(), "no export started");
    let msg = app.view().message.clone().expect("a status line");
    assert!(msg.text.contains("davimci-export-test.mkv"), "{msg:?}");

    // Progress reaches the view state, which is how every frontend shows it.
    app.event(Event::Tick, &mut editor);
    let job = app
        .view()
        .job
        .expect("the export should be the foreground job");
    assert_eq!(job.label, "export");
}

#[test]
fn an_export_finishes_into_a_status_line_and_a_done_job() {
    let (mut app, mut editor) = editor();
    let out = std::env::temp_dir().join("davimci-export-done.mkv");
    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    // The mock backend completes as soon as it is polled enough times.
    for _ in 0..64 {
        app.event(Event::Tick, &mut editor);
        if !editor.exporter().is_running() {
            break;
        }
    }
    assert!(!editor.exporter().is_running(), "the export never finished");
    let job = app
        .jobs()
        .all()
        .iter()
        .find(|j| j.label == "export")
        .expect("an export job");
    assert_eq!(job.state, davimci_app::JobState::Done, "{job:?}");
    // A finished job leaves the status line, not just the job list.
    assert!(
        app.jobs().foreground().is_none(),
        "a finished export should not still be the foreground job"
    );
}

#[test]
fn a_second_export_is_refused_while_one_runs() {
    let (mut app, mut editor) = editor_with_manual_render();
    let a = std::env::temp_dir().join("davimci-busy-a.mkv");
    let b = std::env::temp_dir().join("davimci-busy-b.mkv");
    app.event(
        Event::Command(format!(":export {}", a.display())),
        &mut editor,
    );
    app.event(
        Event::Command(format!(":export {}", b.display())),
        &mut editor,
    );
    let msg = app.view().message.clone().expect("a status line");
    assert!(msg.text.contains("already running"), "{msg:?}");
}

#[test]
fn cancel_stops_a_running_export() {
    let (mut app, mut editor) = editor_with_manual_render();
    let out = std::env::temp_dir().join("davimci-cancel.mkv");
    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    assert!(editor.exporter().is_running());
    app.event(Event::Command(":cancel".into()), &mut editor);
    assert!(!editor.exporter().is_running(), "the export kept running");
    let msg = app.view().message.clone().expect("a status line");
    assert!(msg.text.contains("cancel"), "{msg:?}");
}

#[test]
fn presets_lists_what_render_will_accept() {
    let (mut app, mut editor) = editor();
    app.event(Event::Command(":presets".into()), &mut editor);
    let msg = app.view().message.clone().expect("a status line");
    assert!(msg.text.contains("mkv"), "{msg:?}");
    assert!(msg.text.contains("webm"), "{msg:?}");
}

#[test]
fn render_with_an_unknown_preset_names_the_real_ones() {
    let (mut app, mut editor) = editor();
    app.event(Event::Command(":render youtube".into()), &mut editor);
    assert!(!editor.exporter().is_running());
    let msg = app.view().message.clone().expect("a status line");
    assert!(msg.text.contains("youtube"), "{msg:?}");
    assert!(msg.text.contains("mkv"), "{msg:?}");
}

// ---- the media picker (spec §3.2, `i`/`a`/`r`) --------------------------

/// A prober that invents a 100-frame video file, so the picker path is
/// testable with no ffprobe and no media on disk.
#[derive(Debug)]
struct FakeProber;

impl davimci_analysis::Prober for FakeProber {
    fn probe(
        &self,
        path: &std::path::Path,
    ) -> Result<davimci_analysis::MediaInfo, davimci_analysis::AnalysisError> {
        Ok(davimci_analysis::MediaInfo {
            path: path.to_string_lossy().to_string(),
            duration_seconds: 100.0 / 60.0,
            streams: vec![davimci_analysis::StreamInfo {
                index: 0,
                kind: davimci_analysis::StreamKind::Video,
                codec: "h264".into(),
                title: None,
                language: None,
                fps: Some(Fps::FPS_60),
                resolution: Some(Resolution {
                    width: 8,
                    height: 4,
                }),
                sample_rate: None,
                channels: None,
                frames: Some(100),
                bit_depth: Some(8),
            }],
        })
    }
}

fn picker_editor() -> (App, Editor) {
    let (app, editor) = editor();
    (app, editor.with_prober(Box::new(FakeProber)))
}

/// The whole chain: key -> outcome -> response -> chosen path -> edit.
#[test]
fn pressing_i_asks_the_frontend_for_a_media_picker() {
    let (mut app, mut editor) = picker_editor();
    let response = app.event(Event::Key(Key::Char('i')), &mut editor);
    assert_eq!(
        response,
        davimci_app::Response::OpenPicker(davimci_keys::MediaIntent::Insert),
        "`i` should open the picker, not report NotImplemented"
    );
}

#[test]
fn choosing_media_for_insert_ripples_later_clips_right() {
    let (mut app, mut editor) = picker_editor();
    let before = app.session().timeline().duration();
    app.event(Event::Key(Key::Char('i')), &mut editor);
    app.event(Event::MediaChosen("/m/new.mkv".into()), &mut editor);

    let after = app.session().timeline().duration();
    assert_eq!(
        after.get(),
        before.get() + 100,
        "an insert should lengthen the timeline by the imported media"
    );
    // One command, so one undo takes the whole import back.
    app.event(Event::Key(Key::Char('u')), &mut editor);
    assert_eq!(app.session().timeline().duration(), before);
}

#[test]
fn append_lands_after_the_clip_under_the_playhead_not_at_it() {
    // Otherwise `a` and `i` would be the same key.
    let (mut app, mut editor) = picker_editor();
    app.event(Event::Key(Key::Char('a')), &mut editor);
    app.event(Event::MediaChosen("/m/new.mkv".into()), &mut editor);

    let tl = app.session().timeline();
    let track = tl.track(tl.playhead().track).expect("a focused track");
    let new = track
        .clips()
        .iter()
        .find(|c| c.media.as_ref().is_some_and(|m| m.path == "/m/new.mkv"))
        .expect("the imported clip");
    assert_eq!(new.start.get(), 100, "expected it after the first clip");
}

#[test]
fn replace_with_no_clip_under_the_playhead_is_refused_before_the_picker_opens() {
    // A timeline with a hole in it, so the playhead can sit on nothing.
    let session = Session::new(fixture(&[("V1", &[(0, 100, "a"), (200, 100, "c")])]));
    let mut ws = Workspace::new(std::env::temp_dir()).without_autosave();
    ws.set_current_session(session.clone());
    let presenter = Presenter::new(
        PresentHost::Embedded,
        Resolution {
            width: 32,
            height: 16,
        },
        Fps::FPS_60,
    );
    let mut editor = Editor::new(
        ws,
        Box::new(MockBackend::new(Resolution {
            width: 8,
            height: 4,
        })),
        presenter,
    )
    .with_prober(Box::new(FakeProber));
    let mut app = App::new(session);
    editor.prime(app.session());

    // The next jump point is the far edge of the first clip: the gap.
    app.event(Event::Key(Key::Char('l')), &mut editor);
    assert!(
        app.session()
            .timeline()
            .track(app.session().timeline().playhead().track)
            .and_then(|t| t.clip_at(app.session().timeline().playhead().frame))
            .is_none(),
        "this test needs the playhead over a gap"
    );
    let response = app.event(Event::Key(Key::Char('r')), &mut editor);
    assert_eq!(
        response,
        davimci_app::Response::Continue,
        "the picker should not open for an edit that cannot land"
    );
}

#[test]
fn a_cancelled_picker_changes_nothing() {
    let (mut app, mut editor) = picker_editor();
    let before = app.session().timeline().clone();
    app.event(Event::Key(Key::Char('i')), &mut editor);
    app.event(Event::PickerCancelled, &mut editor);
    assert_eq!(app.session().timeline(), &before, "the timeline moved");

    // And a stray choice afterwards is refused rather than importing.
    app.event(Event::MediaChosen("/m/new.mkv".into()), &mut editor);
    assert_eq!(app.session().timeline(), &before, "a stray file imported");
}
