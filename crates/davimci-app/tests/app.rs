//! App event-loop behaviour with an inline fake frontend (plan.md 9a).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_app::{App, AppError, Event, Frontend, Host, NullHost, Response, Severity, Surface};
use davimci_cmd::Session;
use davimci_core::testing::fixture;
use davimci_core::{Frame, Selection, Timeline};
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
            ..Surface::default()
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
    /// The selection each `:` line arrived with (spec §6.1).
    selections: Vec<Option<Selection>>,
    quit: bool,
}

impl Host for TestHost {
    fn command(
        &mut self,
        line: &str,
        _s: &mut Session,
        selection: Option<&Selection>,
    ) -> Result<Option<String>, AppError> {
        self.commands.push(line.to_string());
        self.selections.push(selection.cloned());
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
        ..Surface::default()
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
        ..Surface::default()
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

/// The line is drawn from the view as it is typed, with a caret and with the
/// candidates for the word under it (idea.md). A frontend that had to keep
/// its own buffer would be a second `:` line.
#[test]
fn the_colon_line_is_visible_as_it_is_typed_and_suggests_completions() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TestHost::default();
    app.set_command_candidates(
        ["b", "bn", "bp", "w"]
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    );
    for k in Key::parse_str(":b") {
        app.key(k, &mut host);
    }
    let line = app.view().command_line.expect("an open line");
    assert_eq!(line.buffer, "b");
    assert_eq!(line.cursor, 1);
    assert_eq!(line.completions, ["b", "bn", "bp"]);

    // Tab completes to the longest common prefix, and the view follows.
    app.key(Key::parse_str("n")[0], &mut host);
    let line = app.view().command_line.expect("an open line");
    assert_eq!(line.buffer, "bn");
    assert!(
        line.completions.is_empty(),
        "a suggestion identical to the line is noise: {:?}",
        line.completions
    );

    // Enter submits what was typed, and the line closes.
    app.key(Key::parse_str("<Enter>")[0], &mut host);
    assert_eq!(host.commands, ["bn"]);
    assert!(app.view().command_line.is_none());
}

/// Esc abandons the line without running it.
#[test]
fn escape_abandons_the_colon_line() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TestHost::default();
    for k in Key::parse_str(":wq<Esc>") {
        app.key(k, &mut host);
    }
    assert!(host.commands.is_empty(), "an abandoned line must not run");
    assert!(app.view().command_line.is_none());
}

#[test]
fn a_colon_line_carries_the_visual_selection_to_the_host() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TestHost::default();
    // `v` then `l` selects a range on V1; `:` clears the selection in the
    // key engine, so the app must have remembered it (spec §6.1).
    for k in Key::parse_str("vl") {
        app.key(k, &mut host);
    }
    let live = app.selection().expect("visual mode has a selection");
    app.key(Key::Char(':'), &mut host);
    assert!(app.selection().is_none(), ": leaves visual mode");
    app.event(Event::Command("gain 3".into()), &mut host);
    assert_eq!(host.selections, [Some(live)]);

    // A second line, typed with nothing selected, must not inherit it.
    app.key(Key::Char(':'), &mut host);
    app.event(Event::Command("gain 3".into()), &mut host);
    assert_eq!(host.selections[1], None);
}

#[test]
fn a_cancelled_colon_line_does_not_leak_its_selection_into_the_next_one() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TestHost::default();
    for k in Key::parse_str("vl:") {
        app.key(k, &mut host);
    }
    app.event(Event::CommandCancelled, &mut host);
    app.key(Key::Char(':'), &mut host);
    app.event(Event::Command("gain 3".into()), &mut host);
    assert_eq!(host.selections, [None]);
}

/// A held `h`/`l` arrives as a burst of repeats in one poll. Every repeat
/// must move the playhead, but the host - which seeks and decodes - is asked
/// once, or the editor spends the whole burst decoding frames nobody sees
/// and appears to freeze (idea.md, spec §14).
#[test]
fn a_burst_of_repeated_keys_seeks_once_and_still_moves_every_step() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 300,
        rows: 4,
        ..Surface::default()
    });
    let mut fe = Recorder {
        // 8 repeats of `l` in one poll, then quit.
        events: Key::parse_str("llllllll")
            .into_iter()
            .map(Event::Key)
            .collect(),
        ..Recorder::default()
    };
    let mut host = TransportHost::default();
    app.run(&mut fe, &mut host).expect("loop runs");
    assert_eq!(
        host.calls.iter().filter(|c| **c == "moved").count(),
        1,
        "one seek per batch, not one per repeat: {:?}",
        host.calls
    );
    assert!(
        app.session().timeline().playhead().frame > Frame::ZERO,
        "the burst still moved the playhead"
    );
}

/// The app asks for pictures of the video clips on screen, nearest the
/// playhead first, and publishes what the host decodes (idea.md).
#[derive(Debug, Default)]
struct ThumbHost {
    asked: Vec<davimci_app::ThumbnailRequest>,
    publish: Vec<(davimci_core::ClipId, davimci_app::Thumbnail)>,
}

impl Host for ThumbHost {
    fn request_thumbnails(&mut self, wanted: &[davimci_app::ThumbnailRequest]) {
        self.asked = wanted.to_vec();
    }

    fn thumbnails(&mut self) -> Vec<(davimci_core::ClipId, davimci_app::Thumbnail)> {
        std::mem::take(&mut self.publish)
    }
}

#[test]
fn a_clip_is_sampled_across_its_width_and_each_picture_is_drawn_where_it_belongs() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 300,
        rows: 4,
        // One picture every 20 columns, so a clip wants several.
        thumbnail_columns: 20,
    });
    // Zoomed in, so a clip is wide enough to hold several pictures.
    app.set_zoom(davimci_motion::Zoom::MAX);
    let mut host = ThumbHost::default();
    app.event(Event::Tick, &mut host);

    assert!(!host.asked.is_empty(), "no clip was asked about");
    assert!(
        host.asked
            .iter()
            .all(|r| app.session().timeline().find_clip(r.clip).is_some()),
        "asked about a clip that is not in the timeline"
    );
    // Audio lanes are never asked about - there is nothing to picture.
    let audio_clips = davimci_core::testing::clip_ids(app.session().timeline(), "A1");
    assert!(
        host.asked.iter().all(|r| !audio_clips.contains(&r.clip)),
        "an audio clip was asked for a picture"
    );

    // A clip is sampled more than once, at different source frames: a
    // filmstrip is the media changing, not one frame repeated.
    let first = host.asked[0];
    let for_clip: Vec<_> = host.asked.iter().filter(|r| r.clip == first.clip).collect();
    assert!(
        for_clip.len() > 1,
        "a clip was asked for one picture, not a strip"
    );
    let sources: std::collections::BTreeSet<u64> =
        for_clip.iter().map(|r| r.source.get()).collect();
    assert_eq!(sources.len(), for_clip.len(), "the same frame twice");
    assert!(
        for_clip.iter().all(|r| r.at
            >= app
                .session()
                .timeline()
                .find_clip(r.clip)
                .expect("clip")
                .1
                .start),
        "a sample landed before the clip it pictures"
    );

    // Nearest the playhead first: the host may only afford one per tick.
    assert!(
        host.asked.iter().all(|r| r.at.get() >= first.at.get()),
        "requests are not ordered outwards from the playhead"
    );

    // Publish two of them, and both reach the view at their own columns.
    host.publish = for_clip
        .iter()
        .take(2)
        .map(|r| {
            (
                r.clip,
                davimci_app::Thumbnail::new(2, 2, vec![255u8; 16], r.source),
            )
        })
        .collect();
    app.event(Event::Tick, &mut host);
    let drawn = app
        .view()
        .tracks
        .iter()
        .flat_map(|t| t.clips.clone())
        .find(|c| c.id == first.clip)
        .expect("the clip is on screen");
    assert_eq!(drawn.thumbnails.len(), 2, "a decoded picture is not drawn");
    let columns: Vec<u32> = drawn.thumbnails.iter().map(|(c, _)| *c).collect();
    assert!(
        columns[0] < columns[1],
        "pictures are not in timeline order: {columns:?}"
    );
    assert_ne!(
        drawn.thumbnails[0].1.source, drawn.thumbnails[1].1.source,
        "two columns of the strip show the same source frame"
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
        ..Surface::default()
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
        ..Surface::default()
    });
    let before = app.viewport();
    let mut host = ImportHost;
    app.key(Key::parse_str("a")[0], &mut host);
    app.event(Event::MediaChosen("clip.mp4".into()), &mut host);
    assert_eq!(app.viewport().zoom(), before.zoom());
}

/// spec §3.2.1: the host is told to drop the clock *before* it is asked to
/// repaint, or the repaint is swallowed by the still-running pacer.
#[derive(Debug, Default)]
struct TransportHost {
    calls: Vec<&'static str>,
}

impl Host for TransportHost {
    fn interrupt_transport(&mut self, _s: &Session) {
        self.calls.push("interrupt");
    }

    fn playhead_moved(&mut self, _s: &Session) {
        self.calls.push("moved");
    }

    fn timeline_changed(&mut self, _s: &Session) {
        self.calls.push("changed");
    }

    fn command(
        &mut self,
        _line: &str,
        _s: &mut Session,
        _selection: Option<&Selection>,
    ) -> Result<Option<String>, AppError> {
        self.calls.push("command");
        Ok(None)
    }
}

#[test]
fn spec_section_3_2_1_a_motion_interrupts_playback_before_repainting() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TransportHost::default();
    app.key(Key::Char('l'), &mut host);
    assert_eq!(host.calls, vec!["interrupt", "moved"]);
}

#[test]
fn spec_section_3_2_1_zoom_and_mode_changes_leave_playback_running() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TransportHost::default();
    for k in Key::parse_str("ziv") {
        app.key(k, &mut host);
    }
    assert!(
        !host.calls.contains(&"interrupt"),
        "zoom/visual took the clock: {:?}",
        host.calls
    );
}

#[test]
fn spec_section_3_2_1_an_ex_command_interrupts_before_it_runs() {
    let mut app = App::new(Session::new(timeline()));
    let mut host = TransportHost::default();
    app.event(Event::Command("w".to_string()), &mut host);
    assert_eq!(host.calls.first(), Some(&"interrupt"));
    assert!(host.calls.contains(&"command"));
}

/// A click seeks: navigation, not an edit (spec §15.2).
#[test]
fn clicking_the_timeline_moves_the_playhead_without_touching_the_undo_log() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 50,
        rows: 3,
        ..Surface::default()
    });
    let undo_before = app.session().history().len();
    app.event(
        Event::Click {
            column: 10,
            row: None,
        },
        &mut NullHost,
    );
    let head = app.session().timeline().playhead();
    let duration = app.session().timeline().duration();
    let want = app
        .viewport()
        .frame_at_column(10)
        .min(Frame(duration.get() - 1));
    assert_eq!(head.frame, want, "the click landed on its own column");
    assert_eq!(
        app.session().history().len(),
        undo_before,
        "seeking is not an edit"
    );
}

/// Clicking a lane focuses that track, which is what makes click-to-seek
/// usable on a multi-track timeline.
#[test]
fn clicking_a_lane_focuses_that_track() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 50,
        rows: 3,
        ..Surface::default()
    });
    let want = app.session().timeline().tracks()[1].id;
    app.event(
        Event::Click {
            column: 5,
            row: Some(1),
        },
        &mut NullHost,
    );
    assert_eq!(app.session().timeline().playhead().track, want);
}

/// A click past the end of the timeline lands on the last frame rather than
/// somewhere no frame exists.
#[test]
fn a_click_past_the_end_clamps_to_the_last_frame() {
    let mut app = App::new(Session::new(timeline()));
    app.resize(Surface {
        columns: 50,
        rows: 3,
        ..Surface::default()
    });
    let duration = app.session().timeline().duration();
    app.event(
        Event::Click {
            column: 49,
            row: None,
        },
        &mut NullHost,
    );
    assert!(app.session().timeline().playhead().frame < duration);
}

/// Spec §15.4: INSERT on a subtitle clip edits text, and Esc commits it as an
/// ordinary undoable command.
#[test]
fn a_committed_subtitle_edit_is_one_undoable_command() {
    let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("T1", &[])]);
    let t1 = davimci_core::testing::track_id(&tl, "T1");
    let id = tl.new_clip_id();
    let mut clip = davimci_core::Clip::generated(id, "sub", Frame::ZERO, Frame(50));
    clip.text = Some("hello".into());
    tl.restore(t1, Frame::ZERO, &[clip], Frame(50), false)
        .unwrap();
    let mut session = Session::new(tl);
    session.set_playhead(Frame::ZERO, t1).unwrap();
    let mut app = App::new(session);

    let response = app.key(Key::parse_str("i").remove(0), &mut NullHost);
    assert_eq!(
        response,
        Response::EditText {
            clip: id,
            text: "hello".into()
        }
    );

    app.event(
        Event::TextEdited {
            clip: id,
            text: "goodbye".into(),
        },
        &mut NullHost,
    );
    let text = app
        .session()
        .timeline()
        .find_clip(id)
        .map(|(_, c)| c.text.clone().unwrap_or_default());
    assert_eq!(text.as_deref(), Some("goodbye"));

    app.key(Key::parse_str("u").remove(0), &mut NullHost);
    let text = app
        .session()
        .timeline()
        .find_clip(id)
        .map(|(_, c)| c.text.clone().unwrap_or_default());
    assert_eq!(text.as_deref(), Some("hello"), "the edit was undoable");
}

/// Text arriving with no editor open is refused: it is a write nobody asked
/// for.
#[test]
fn unsolicited_text_is_refused() {
    let mut app = App::new(Session::new(timeline()));
    app.event(
        Event::TextEdited {
            clip: davimci_core::ClipId(1),
            text: "x".into(),
        },
        &mut NullHost,
    );
    let msg = app.view().message.expect("a message");
    assert_eq!(msg.severity, Severity::Error);
}
