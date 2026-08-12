//! Project-lifecycle tests.
//!
//! Everything here runs against a scratch directory under the system temp
//! dir; no fixture media is needed, because the lifecycle layer only ever
//! sees timelines and files.

#![allow(
    clippy::float_cmp,
    reason = "the values under test are set exactly, so exact equality is the assertion"
)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use davimci_cmd::{EditCommand, ProjectFile, Session};
use davimci_core::testing::{fixture, media_fixture, track_id};
use davimci_core::{Frame, Register, Timeline};

use crate::autosave::{self, OnRecovery};
use crate::error::CliError;
use crate::excmd::{ExCommand, ExOutcome, parse};
use crate::workspace::Workspace;
use davimci_core::{Classify, ErrorClass};

/// A scratch directory that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("davimci-test-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn split(tl: &Timeline, frame: u64) -> EditCommand {
    EditCommand::Split {
        track: track_id(tl, "V1"),
        frame: Frame(frame),
        new_id: None,
    }
}

fn seeded(ws: &mut Workspace, tl: Timeline) {
    ws.adopt(Session::new(tl), None);
}

fn json(tl: &Timeline) -> String {
    serde_json::to_string(tl).unwrap()
}

// parsing

#[test]
fn the_ex_grammar_covers_the_spec_12_table() {
    let cases: &[(&str, ExCommand)] = &[
        (":w", ExCommand::Write(None)),
        (
            ":w out.davimci",
            ExCommand::Write(Some("out.davimci".into())),
        ),
        (":q", ExCommand::Quit { force: false }),
        (":q!", ExCommand::Quit { force: true }),
        (":wq", ExCommand::WriteQuit(None)),
        (":x", ExCommand::WriteQuit(None)),
        (":e cut.davimci", ExCommand::Edit("cut.davimci".into())),
        // Regression: a path argument is the rest of the line. Splitting on
        // whitespace made `:e stupid brig scrim.mkv` a usage error, which is
        // most media files on a real disk.
        (
            ":e /m/stupid brig scrim.mkv",
            ExCommand::Edit("/m/stupid brig scrim.mkv".into()),
        ),
        (
            ":w /m/my project.davimci",
            ExCommand::Write(Some("/m/my project.davimci".into())),
        ),
        (":new", ExCommand::New),
        (":ls", ExCommand::List),
        (":bn", ExCommand::BufferNext),
        (":bp", ExCommand::BufferPrev),
        (":b 2", ExCommand::Buffer(2)),
        (
            ":relink /new/a.mkv",
            ExCommand::Relink {
                old: None,
                new: "/new/a.mkv".into(),
            },
        ),
        (":group", ExCommand::Group),
        (":ungroup", ExCommand::Ungroup),
        (
            ":track text",
            ExCommand::AddTrack {
                kind: davimci_core::TrackKind::Text,
                name: None,
            },
        ),
        (
            ":track A Music",
            ExCommand::AddTrack {
                kind: davimci_core::TrackKind::Audio,
                name: Some("Music".into()),
            },
        ),
        (":track!", ExCommand::RemoveTrack),
        (
            ":track move up",
            ExCommand::MoveTrack(crate::excmd::TrackMove::Up),
        ),
        (
            ":track move bottom",
            ExCommand::MoveTrack(crate::excmd::TrackMove::Bottom),
        ),
        // Positions the user types are one-based.
        (
            ":track move 2",
            ExCommand::MoveTrack(crate::excmd::TrackMove::To(1)),
        ),
        // A cue is a sentence, so it is the rest of the line, spaces and all.
        (
            ":text and then he said hello",
            ExCommand::Text("and then he said hello".into()),
        ),
        // `:subtitle` predates `:text` and still writes the same cue, because
        // a subtitle track is a text track.
        (
            ":subtitle a subtitle line",
            ExCommand::Text("a subtitle line".into()),
        ),
        (
            ":sub a subtitle line",
            ExCommand::Text("a subtitle line".into()),
        ),
        (
            ":relink /old/a.mkv /new/a.mkv",
            ExCommand::Relink {
                old: Some("/old/a.mkv".into()),
                new: "/new/a.mkv".into(),
            },
        ),
    ];
    for (line, expected) in cases {
        assert_eq!(&parse(line).unwrap(), expected, "parsing {line}");
        // The colon is optional, so a frontend may hand us either form.
        assert_eq!(&parse(line.trim_start_matches(':')).unwrap(), expected);
    }
}

#[test]
fn unknown_and_misused_commands_are_user_errors_with_a_sentence() {
    for line in [
        ":wat",
        ":e",
        ":b",
        ":relink",
        ":track",
        ":track nonsense",
        ":track move",
        ":track move sideways",
        ":track move 0",
        ":text",
    ] {
        let err = parse(line).unwrap_err();
        assert_eq!(err.class(), ErrorClass::User, "{line}");
        assert!(!err.user_message().is_empty(), "{line}");
    }
}

// save / load

#[test]
fn saving_and_reopening_gives_a_byte_identical_timeline() {
    let dir = Scratch::new("roundtrip");
    let file = dir.join("cut.davimci");

    let mut ws = Workspace::new(dir.path());
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();
    let cmd = split(ws.current().timeline(), 200);
    ws.exec(&cmd).unwrap();
    let before = json(ws.current().timeline());
    ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
        .unwrap();

    let mut reopened = Workspace::new(dir.path());
    reopened
        .run(&format!("e {}", file.display()), OnRecovery::Discard)
        .unwrap();
    assert_eq!(json(reopened.current().timeline()), before);
    assert!(!reopened.current().is_dirty());
}

#[test]
fn a_project_written_by_an_older_schema_still_opens() {
    // Format migration. Version 0 is pre-release: no
    // `version` field and no log.
    let dir = Scratch::new("migrate");
    let file = dir.join("old.davimci");
    let old = serde_json::json!({
        "snapshot": serde_json::to_value(fixture(&[("V1", &[(0, 120, "a")])])).unwrap(),
    });
    std::fs::write(&file, serde_json::to_string(&old).unwrap()).unwrap();

    let mut ws = Workspace::new(dir.path());
    ws.run(&format!("e {}", file.display()), OnRecovery::Discard)
        .unwrap();
    assert_eq!(
        ws.current().timeline().dump(),
        "V1:[a 0-120]\nA1: -\n",
        "an old document must migrate, not fail"
    );
}

#[test]
fn writing_without_a_filename_is_refused_rather_than_guessed() {
    let dir = Scratch::new("noname");
    let mut ws = Workspace::new(dir.path());
    assert!(matches!(
        ws.run("w", OnRecovery::Discard),
        Err(CliError::NoFilename)
    ));
}

// dirty state

#[test]
fn quit_refuses_on_unsaved_changes_and_quit_bang_discards() {
    let dir = Scratch::new("dirty");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    assert!(!ws.current().is_dirty());

    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();
    assert!(ws.current().is_dirty());
    assert!(matches!(
        ws.run("q", OnRecovery::Discard),
        Err(CliError::UnsavedChanges)
    ));
    assert_eq!(ws.buffers().len(), 2, "a refused :q closes nothing");

    assert!(ws.run("q!", OnRecovery::Discard).is_ok());
    assert_eq!(ws.buffers().len(), 1);
}

/// A prober that answers with one video stream, so import is testable with
/// no media and no ffprobe.
#[derive(Debug)]
struct StubProber;

impl davimci_analysis::Prober for StubProber {
    fn probe(
        &self,
        path: &Path,
    ) -> Result<davimci_analysis::MediaInfo, davimci_analysis::AnalysisError> {
        Ok(davimci_analysis::MediaInfo {
            path: path.display().to_string(),
            duration_seconds: 10.0,
            streams: vec![davimci_analysis::StreamInfo {
                index: 0,
                kind: davimci_analysis::StreamKind::Video,
                codec: "h264".into(),
                title: None,
                language: None,
                fps: Some(davimci_core::Fps::new(25, 1).unwrap()),
                resolution: Some(davimci_core::Resolution {
                    width: 1920,
                    height: 1080,
                }),
                sample_rate: None,
                channels: None,
                frames: Some(250),
                bit_depth: Some(8),
            }],
        })
    }
}

#[test]
fn a_freshly_imported_clip_quits_without_force() {
    let dir = Scratch::new("importquit");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    ws.import_media(&dir.join("clip.mkv"), &StubProber).unwrap();
    assert!(
        !ws.current().is_dirty(),
        "an import is not an edit the user made"
    );
    assert!(ws.run("q", OnRecovery::Discard).is_ok());

    ws.import_media(&dir.join("clip.mkv"), &StubProber).unwrap();
    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();
    assert!(
        matches!(
            ws.run("q", OnRecovery::Discard),
            Err(CliError::UnsavedChanges)
        ),
        "editing the imported clip makes it worth saving"
    );
}

#[test]
fn undoing_back_to_the_saved_state_is_clean_again() {
    let dir = Scratch::new("undirty");
    let file = dir.join("p.davimci");
    let mut ws = Workspace::new(dir.path());
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
        .unwrap();

    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();
    assert!(ws.current().is_dirty());
    ws.with_session(davimci_cmd::Session::undo).unwrap();
    assert!(
        !ws.current().is_dirty(),
        "dirty is a comparison with the saved state, not a sticky flag"
    );
}

#[test]
fn quitting_the_last_timeline_ends_the_session() {
    let dir = Scratch::new("lastquit");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    assert_eq!(ws.run("q", OnRecovery::Discard).unwrap(), ExOutcome::Quit);
    assert!(ws.should_quit());
}

// buffers

#[test]
fn buffers_list_and_switch_like_vim() {
    let dir = Scratch::new("buffers");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    ws.run("new", OnRecovery::Discard).unwrap();
    ws.run("new", OnRecovery::Discard).unwrap();
    let listed = ws.list();
    assert_eq!(listed.len(), 3);
    assert!(listed[2].contains('%'), "the current buffer is marked");

    let first = ws.buffers()[0].id();
    ws.run(&format!("b {first}"), OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().id(), first);
    ws.run("bn", OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().id(), ws.buffers()[1].id());
    ws.run("bp", OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().id(), first);
    ws.run("bp", OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().id(), ws.buffers()[2].id(), "bp wraps");

    assert!(matches!(
        ws.run("b 99", OnRecovery::Discard),
        Err(CliError::NoSuchBuffer(_))
    ));
}

#[test]
fn registers_and_marks_are_global_across_timelines() {
    // Registers and marks are global across timelines, so a yank
    // in one can be pasted into another".
    let dir = Scratch::new("globals");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, fixture(&[("V1", &[(0, 200, "a")])]));

    let yanked = Register {
        clips: vec![davimci_core::Clip::generated(
            davimci_core::ClipId(0),
            "y",
            Frame::ZERO,
            Frame(50),
        )],
        span: Frame(50),
    };
    ws.with_session(|s| {
        s.set_register('a', yanked.clone());
        s.set_mark('m', Frame(120), None);
    });

    ws.run("new", OnRecovery::Discard).unwrap();
    assert_eq!(
        ws.current().timeline().registers.get(&'a'),
        Some(&yanked),
        "the yank must be visible in the new timeline"
    );
    assert_eq!(
        ws.current().timeline().marks.get(&'m').map(|m| m.frame),
        Some(Frame(120))
    );

    // ... and it pastes there.
    let track = track_id(ws.current().timeline(), "V1");
    ws.exec(&EditCommand::Paste {
        track,
        at: Frame::ZERO,
        register: yanked,
        ripple: true,
    })
    .unwrap();
    assert_eq!(ws.current().timeline().dump(), "V1:[y 0-50]\nA1: -\n");
}

// autosave and crash recovery

#[test]
fn autosave_never_touches_the_project_file() {
    let dir = Scratch::new("autosave-scope");
    let file = dir.join("p.davimci");
    let mut ws = Workspace::new(dir.path());
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
        .unwrap();
    let on_disk = std::fs::read_to_string(&file).unwrap();

    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        on_disk,
        "an edit must not write through to the project file"
    );
    assert!(
        ws.autosave_dir()
            .join(ws.autosave_path_for(&file).file_name().unwrap())
            .exists()
    );
}

#[test]
fn a_crashed_session_replays_to_the_exact_pre_crash_state() {
    let dir = Scratch::new("recover");
    let file = dir.join("p.davimci");

    let expected = {
        let mut ws = Workspace::new(dir.path());
        seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
        ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
            .unwrap();
        for frame in [100, 200, 250] {
            let cmd = split(ws.current().timeline(), frame);
            ws.exec(&cmd).unwrap();
        }
        json(ws.current().timeline())
        // The workspace is dropped without :w or :q - the crash.
    };

    let saved = std::fs::read_to_string(&file).unwrap();
    let mut ws = Workspace::new(dir.path());
    let pending = ws
        .pending_recovery(&file)
        .expect("an autosave must survive");
    assert_eq!(pending.commands, 3);

    ws.open_project(&file, OnRecovery::Recover).unwrap();
    assert_eq!(
        json(ws.current().timeline()),
        expected,
        "recovery must land on the pre-crash timeline exactly"
    );
    assert!(
        ws.current().is_dirty(),
        "a recovered timeline is ahead of the file, so it is unsaved"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        saved,
        "recovery must not have rewritten the project"
    );
}

#[test]
fn declining_recovery_opens_the_saved_state_and_clears_the_log() {
    let dir = Scratch::new("decline");
    let file = dir.join("p.davimci");
    let saved_dump;
    {
        let mut ws = Workspace::new(dir.path());
        seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
        ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
            .unwrap();
        saved_dump = ws.current().timeline().dump();
        let cmd = split(ws.current().timeline(), 100);
        ws.exec(&cmd).unwrap();
    }

    let mut ws = Workspace::new(dir.path());
    ws.open_project(&file, OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().timeline().dump(), saved_dump);
    assert!(!ws.current().is_dirty());
    assert!(
        ws.pending_recovery(&file).is_none(),
        "a declined autosave is discarded, not left to prompt again"
    );
}

#[test]
fn saving_clears_the_autosave_so_the_next_open_does_not_prompt() {
    let dir = Scratch::new("clear");
    let file = dir.join("p.davimci");
    let mut ws = Workspace::new(dir.path());
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
        .unwrap();
    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();
    assert!(ws.pending_recovery(&file).is_some());
    ws.run("w", OnRecovery::Discard).unwrap();
    assert!(ws.pending_recovery(&file).is_none());
}

#[test]
fn the_autosave_describes_the_state_after_an_undo() {
    let dir = Scratch::new("undo-log");
    let file = dir.join("p.davimci");
    let mut ws = Workspace::new(dir.path());
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
        .unwrap();
    for frame in [100, 200] {
        let cmd = split(ws.current().timeline(), frame);
        ws.exec(&cmd).unwrap();
    }
    ws.with_session(davimci_cmd::Session::undo).unwrap();

    let log = ws.autosave_path_for(&file);
    let recovered = autosave::replay(&log).unwrap();
    assert_eq!(
        recovered.timeline().dump(),
        ws.current().timeline().dump(),
        "the log must describe the state after the undo"
    );
}

#[test]
fn a_corrupt_autosave_is_reported_rather_than_partially_applied() {
    let dir = Scratch::new("corrupt");
    let log = dir.join("broken.log");
    std::fs::write(&log, "not json at all\n").unwrap();
    let err = autosave::replay(&log).unwrap_err();
    assert_eq!(err.class(), ErrorClass::Corruption);
    assert!(autosave::inspect(&log).is_none());
}

// relink

#[test]
fn relink_brings_offline_media_back_and_is_undoable() {
    let dir = Scratch::new("relink");
    let media = dir.join("found.mkv");
    std::fs::write(&media, b"not really a movie").unwrap();

    let mut ws = Workspace::new(dir.path()).without_autosave();
    let mut tl = media_fixture(&[(0, 100, 0, 200), (100, 100, 0, 200)]);
    let clips: Vec<_> = tl.tracks()[0].clips().iter().map(|c| c.id).collect();
    let old_path = tl
        .find_clip(clips[0])
        .unwrap()
        .1
        .media
        .as_ref()
        .unwrap()
        .path
        .clone();
    // Both clips come off the same source file, as two halves of one shot do.
    tl.set_media_source(clips[1], old_path.clone(), true)
        .unwrap();
    for c in &clips {
        tl.set_media_offline(*c, true).unwrap();
    }
    seeded(&mut ws, tl);

    let out = ws
        .run(
            &format!("relink {old_path} {}", media.display()),
            OnRecovery::Discard,
        )
        .unwrap();
    assert!(matches!(out, ExOutcome::Message(ref m) if m.contains("2 clip")));
    for c in &clips {
        let (_, clip) = ws.current().timeline().find_clip(*c).unwrap();
        assert_eq!(
            clip.media.as_ref().unwrap().path,
            media.display().to_string()
        );
        assert!(!clip.is_offline(), "the file exists, so it is online again");
    }

    // One undo step, because the relink was one Sequence.
    ws.with_session(davimci_cmd::Session::undo).unwrap();
    for c in &clips {
        let (_, clip) = ws.current().timeline().find_clip(*c).unwrap();
        assert_eq!(clip.media.as_ref().unwrap().path, old_path);
        assert!(clip.is_offline());
    }
}

#[test]
fn relinking_to_a_missing_file_keeps_the_clip_offline_and_says_so() {
    let dir = Scratch::new("relink-missing");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, media_fixture(&[(0, 100, 0, 200)]));
    let clip = ws.current().timeline().tracks()[0].clips()[0].id;

    let out = ws
        .run("relink /nowhere/gone.mkv", OnRecovery::Discard)
        .unwrap();
    assert!(matches!(out, ExOutcome::Message(ref m) if m.contains("still missing")));
    let (_, c) = ws.current().timeline().find_clip(clip).unwrap();
    assert!(c.is_offline(), "export must stay blocked (Phase 0 policy)");
}

#[test]
fn relink_with_no_matching_clip_is_a_user_error() {
    let dir = Scratch::new("relink-none");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, media_fixture(&[(0, 100, 0, 200)]));
    assert!(matches!(
        ws.run("relink /no/such.mkv /other.mkv", OnRecovery::Discard),
        Err(CliError::NoClipUsesPath(_))
    ));
    // A generated clip has no media, so there is nothing to relink under the
    // playhead.
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, fixture(&[("V1", &[(0, 100, "a")])]));
    assert!(matches!(
        ws.run("relink /x.mkv", OnRecovery::Discard),
        Err(CliError::NothingToRelink)
    ));
}

// :e dispatch

#[test]
fn edit_of_a_missing_file_fails_without_opening_a_buffer() {
    let dir = Scratch::new("missing");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    assert!(ws.run("e /no/such/project", OnRecovery::Discard).is_err());
    assert_eq!(ws.buffers().len(), 1);
}

#[test]
fn a_project_file_is_recognised_by_its_content_not_its_name() {
    let dir = Scratch::new("sniff");
    let odd = dir.join("weird-name");
    let text = ProjectFile::from_session(&Session::new(fixture(&[("V1", &[(0, 60, "a")])])))
        .to_json()
        .unwrap();
    std::fs::write(&odd, text).unwrap();

    let mut ws = Workspace::new(dir.path()).without_autosave();
    ws.run(&format!("e {}", odd.display()), OnRecovery::Discard)
        .unwrap();
    assert_eq!(ws.current().timeline().dump(), "V1:[a 0-60]\nA1: -\n");
}

// jumping

#[test]
fn a_bare_number_is_a_frame_to_jump_to_and_clamps_to_the_timeline() {
    let dir = Scratch::new("goto");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    let tl = media_fixture(&[(0, 100, 20, 400)]);
    seeded(&mut ws, tl);

    assert_eq!(parse(":42").unwrap(), ExCommand::Goto(Frame(42)));
    ws.run(":42", OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().timeline().playhead().frame, Frame(42));

    // Past the end lands on the end rather than off it.
    ws.run(":99999", OnRecovery::Discard).unwrap();
    let duration = ws.current().timeline().duration();
    assert_eq!(ws.current().timeline().playhead().frame, duration);
}

// completion

#[test]
fn set_completes_enumerated_values_and_shows_a_free_form_setting_current_value() {
    use crate::setting::{CurrentSettings, PreviewHeight};

    let current = CurrentSettings {
        preview_height: Some(PreviewHeight::Percent(50)),
        numbers: Some(davimci_app::Numbers::Relative),
        ..CurrentSettings::default()
    };
    let v = crate::excmd::vocabulary_with(&current);
    assert_eq!(v.candidates("set numbers"), davimci_app::Numbers::NAMES);
    assert_eq!(
        v.candidates("set previewheight"),
        ["50%".to_string(), "auto".to_string()],
        "a free-form setting offers what it currently holds"
    );
    assert_eq!(v.candidates("set preview"), ["on", "off"]);
}

// transitions

#[test]
fn transition_command_adds_replaces_and_removes_at_the_nearest_cut() {
    let dir = Scratch::new("transition");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    let tl = media_fixture(&[(0, 100, 20, 400), (100, 100, 20, 400)]);
    let right = tl.tracks()[0].clips()[1].id;
    seeded(&mut ws, tl);

    let at = |ws: &Workspace| {
        ws.current()
            .timeline()
            .find_clip(right)
            .and_then(|(_, c)| c.transition_in.clone())
    };

    // Naming no type is a usage error: no type is core, so there is
    // nothing for `:transition` to fall back to.
    assert!(ws.run("transition", OnRecovery::Discard).is_err());

    ws.run("transition dissolve", OnRecovery::Discard).unwrap();
    assert_eq!(
        at(&ws).map(|t| (t.kind, t.duration.get())),
        Some(("dissolve".into(), 12))
    );

    // Re-running replaces, which is how a type or duration is changed.
    ws.run("transition wipe_left 20", OnRecovery::Discard)
        .unwrap();
    assert_eq!(
        at(&ws).map(|t| (t.kind, t.duration.get())),
        Some(("wipe_left".into(), 20))
    );

    ws.run("transition none", OnRecovery::Discard).unwrap();
    assert_eq!(at(&ws), None);
    // Nothing left to remove is a user error with a sentence, not a panic.
    assert!(ws.run("transition none", OnRecovery::Discard).is_err());
}

#[test]
fn a_transition_longer_than_the_handles_is_refused_intact() {
    let dir = Scratch::new("transition-handles");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(
        &mut ws,
        media_fixture(&[(0, 100, 5, 110), (100, 100, 5, 110)]),
    );
    let before = ws.current().timeline().clone();
    let err = ws
        .run("transition dissolve 40", OnRecovery::Discard)
        .unwrap_err();
    assert!(err.to_string().contains("handle"), "{err}");
    assert_eq!(ws.current().timeline(), &before);
}

// `:set`

#[test]
fn set_writes_the_property_and_one_undo_takes_it_back() {
    let dir = Scratch::new("set-clip");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    let tl = media_fixture(&[(0, 100, 20, 400), (100, 100, 20, 400)]);
    seeded(&mut ws, tl);
    let before = json(ws.current().timeline());
    let props = |ws: &Workspace| ws.current().timeline().tracks()[0].clips()[0].props;

    for (line, check) in [
        (":set clip.scale 0.5", 0),
        (":set clip.opacity 0.25", 1),
        (":set clip.x -40", 2),
        (":set clip.gain -6", 3),
        (":set clip.fade_in 100", 4),
    ] {
        ws.run(line, OnRecovery::Discard).unwrap();
        let p = props(&ws);
        match check {
            0 => assert!((p.transform.scale - 0.5).abs() < f32::EPSILON),
            1 => assert!((p.transform.opacity - 0.25).abs() < f32::EPSILON),
            2 => assert!((p.transform.x + 40.0).abs() < f32::EPSILON),
            3 => assert!((p.gain_db + 6.0).abs() < f32::EPSILON),
            _ => assert!(p.fade_in.get() > 0),
        }
    }
    // Each setter is one command, so one undo per `:set` returns exactly.
    for _ in 0..5 {
        ws.with_session(|s| s.undo().map(|_| ())).unwrap();
    }
    assert_eq!(json(ws.current().timeline()), before);
}

#[test]
fn a_rejected_set_leaves_the_timeline_byte_identical() {
    let dir = Scratch::new("set-reject");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, media_fixture(&[(0, 100, 20, 400)]));
    let before = json(ws.current().timeline());
    for line in [
        ":set clip.opacity 2",
        ":set clip.wobble 1",
        ":set transition.duration 0",
        ":set timeline.fps nope",
        ":set clip.scale",
    ] {
        let err = ws.run(line, OnRecovery::Discard).unwrap_err();
        assert_eq!(
            davimci_core::Classify::class(&err),
            davimci_core::ErrorClass::User,
            "{line}"
        );
        assert_eq!(json(ws.current().timeline()), before, "{line}");
    }
}

#[test]
fn set_transition_changes_type_and_duration_without_re_running_transition() {
    let dir = Scratch::new("set-transition");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    let tl = media_fixture(&[(0, 100, 20, 400), (100, 100, 20, 400)]);
    let right = tl.tracks()[0].clips()[1].id;
    seeded(&mut ws, tl);
    let at = |ws: &Workspace| {
        ws.current()
            .timeline()
            .find_clip(right)
            .and_then(|(_, c)| c.transition_in.clone())
            .map(|t| (t.kind, t.duration.get()))
    };

    // With no transition on the cut there is nothing to change.
    assert!(
        ws.run(":set transition.duration 20", OnRecovery::Discard)
            .is_err()
    );
    ws.run("transition dissolve", OnRecovery::Discard).unwrap();
    ws.run(":set transition.duration 20", OnRecovery::Discard)
        .unwrap();
    assert_eq!(at(&ws), Some(("dissolve".into(), 20)));
    ws.run(":set transition.type wipe_left", OnRecovery::Discard)
        .unwrap();
    assert_eq!(at(&ws), Some(("wipe_left".into(), 20)));
}

#[test]
fn set_timeline_fps_is_the_exactly_invertible_reconform() {
    let dir = Scratch::new("set-fps");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(
        &mut ws,
        media_fixture(&[(0, 120, 20, 400), (120, 60, 20, 400)]),
    );
    let before = json(ws.current().timeline());
    ws.run(":set timeline.fps 30", OnRecovery::Discard).unwrap();
    assert_eq!(ws.current().timeline().props.fps.as_f64(), 30.0);
    ws.with_session(|s| s.undo().map(|_| ())).unwrap();
    assert_eq!(json(ws.current().timeline()), before);
}

#[test]
fn a_transform_set_through_set_projects_the_same_xml_as_one_set_in_model() {
    let dir = Scratch::new("set-xml");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    let tl = media_fixture(&[(0, 100, 20, 400)]);
    let (track, clip) = (track_id(&tl, "V1"), tl.tracks()[0].clips()[0].clone());
    seeded(&mut ws, tl.clone());
    ws.run(":set clip.scale 0.5", OnRecovery::Discard).unwrap();
    ws.run(":set clip.opacity 0.25", OnRecovery::Discard)
        .unwrap();

    let mut model = Session::new(tl);
    model
        .exec(&EditCommand::SetProps {
            track,
            clip: clip.id,
            props: davimci_core::ClipProps {
                transform: davimci_core::Transform {
                    scale: 0.5,
                    opacity: 0.25,
                    ..clip.props.transform
                },
                ..clip.props
            },
        })
        .unwrap();
    assert_eq!(
        davimci_mlt::to_xml(&davimci_mlt::Projection::of(ws.current().timeline())),
        davimci_mlt::to_xml(&davimci_mlt::Projection::of(model.timeline()))
    );
}

/// Recovery rebuilds the *tree*, not a line: a branch abandoned before the
/// crash is still reachable with `g-`/`g+` afterwards.
#[test]
fn recovery_restores_the_undo_tree_with_its_branches() {
    let dir = Scratch::new("recover-tree");
    let file = dir.join("p.davimci");

    let (before_list, expected) = {
        let mut ws = Workspace::new(dir.path());
        seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
        ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
            .unwrap();
        // One branch...
        for frame in [100, 200] {
            let cmd = split(ws.current().timeline(), frame);
            ws.exec(&cmd).unwrap();
        }
        // ...then back up and start another, which is what makes a tree.
        ws.with_session(davimci_cmd::Session::undo).unwrap();
        let cmd = split(ws.current().timeline(), 250);
        ws.exec(&cmd).unwrap();
        (
            ws.current_session().undolist().len(),
            json(ws.current().timeline()),
        )
        // Dropped without :w or :q - the crash.
    };

    let mut ws = Workspace::new(dir.path());
    ws.open_project(&file, OnRecovery::Recover).unwrap();
    assert_eq!(json(ws.current().timeline()), expected);
    assert_eq!(
        ws.current_session().undolist().len(),
        before_list,
        "the abandoned branch did not survive recovery"
    );

    // `g-` walks back in change order and reaches the abandoned branch's
    // state, exactly as it did before the crash.
    let seen: Vec<String> = (0..3)
        .map(|_| {
            ws.with_session(|s| s.time_travel_back().map(|_| ()))
                .unwrap();
            ws.current().timeline().dump()
        })
        .collect();
    assert!(
        seen.iter().any(|d| d.contains("100")),
        "the other branch was not reachable: {seen:?}"
    );
}

/// A crash mid-write leaves half a record. Everything before it recovers.
#[test]
fn a_truncated_final_record_recovers_everything_before_it() {
    let dir = Scratch::new("truncated");
    let file = dir.join("p.davimci");
    let mut ws = Workspace::new(dir.path());
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    ws.run(&format!("w {}", file.display()), OnRecovery::Discard)
        .unwrap();
    for frame in [100, 200] {
        let cmd = split(ws.current().timeline(), frame);
        ws.exec(&cmd).unwrap();
    }
    let expected = ws.current().timeline().dump();

    // Cut the file mid-record, as a crash between two writes would.
    let log = ws.autosave_path_for(&file);
    let text = std::fs::read_to_string(&log).unwrap();
    std::fs::write(&log, format!("{text}{{\"n\":{{\"id\":9,\"par")).unwrap();

    let recovered = autosave::replay(&log).unwrap();
    assert_eq!(
        recovered.timeline().dump(),
        expected,
        "the complete records before the torn one must still replay"
    );
}

// grouping

/// `:group` links the column under the playhead; `:ungroup` breaks the group
/// again, so a member can be edited on its own.
#[test]
fn group_and_ungroup_bracket_a_link() {
    let dir = Scratch::new("group");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(
        &mut ws,
        fixture(&[("V1", &[(0, 300, "a")]), ("A1", &[(0, 300, "a")])]),
    );

    assert_eq!(
        ws.run("group", OnRecovery::Discard).unwrap(),
        ExOutcome::Message("2 clips grouped".into())
    );
    let groups = |ws: &Workspace| -> Vec<Option<davimci_core::GroupId>> {
        ws.current()
            .timeline()
            .tracks()
            .iter()
            .flat_map(davimci_core::Track::clips)
            .map(|c| c.group)
            .collect()
    };
    let linked = groups(&ws);
    assert_eq!(linked.len(), 2);
    assert!(linked[0].is_some() && linked[0] == linked[1]);

    assert_eq!(
        ws.run("ungroup", OnRecovery::Discard).unwrap(),
        ExOutcome::Message("2 clips ungrouped".into())
    );
    assert_eq!(groups(&ws), vec![None, None]);
}

// text tracks

/// A text track can be made from nothing and written into: import is not
/// the only way to get one, and a track with no cues is not a dead end.
#[test]
fn a_text_track_can_be_created_and_written_into() {
    let dir = Scratch::new("texttrack");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));

    assert_eq!(
        ws.run("track text", OnRecovery::Discard).unwrap(),
        ExOutcome::Message("added track T1".into())
    );
    let t1 = davimci_core::testing::track_id(ws.current().timeline(), "T1");

    // A cue needs the text track focused; asking for one on V1 is refused
    // before anything is written.
    let err = ws.run("text hello", OnRecovery::Discard).unwrap_err();
    assert_eq!(err.class(), ErrorClass::User);
    assert!(
        ws.current()
            .timeline()
            .track(t1)
            .unwrap()
            .clips()
            .is_empty(),
        "a refused cue still wrote to the timeline"
    );

    ws.with_session(|s| s.set_playhead(davimci_core::Frame::ZERO, t1))
        .unwrap();
    ws.run("text hello there", OnRecovery::Discard).unwrap();
    let clips = ws.current().timeline().track(t1).unwrap().clips().to_vec();
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].text.as_deref(), Some("hello there"));
    assert!(clips[0].media.is_none(), "a cue is a generated clip");

    // And one undo takes the cue back out.
    ws.with_session(davimci_cmd::Session::undo).unwrap();
    assert!(
        ws.current()
            .timeline()
            .track(t1)
            .unwrap()
            .clips()
            .is_empty()
    );
}

/// An empty track can be removed again; one with clips on it cannot, so
/// `:track!` is never a quiet way to lose an edit.
#[test]
fn only_an_empty_track_can_be_removed() {
    let dir = Scratch::new("rmtrack");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    let v1 = davimci_core::testing::track_id(ws.current().timeline(), "V1");
    ws.with_session(|s| s.set_playhead(davimci_core::Frame::ZERO, v1))
        .unwrap();

    let err = ws.run("track!", OnRecovery::Discard).unwrap_err();
    assert!(!err.user_message().is_empty());
    assert!(ws.current().timeline().track(v1).is_some());

    ws.run("track text", OnRecovery::Discard).unwrap();
    let t1 = davimci_core::testing::track_id(ws.current().timeline(), "T1");
    ws.with_session(|s| s.set_playhead(davimci_core::Frame::ZERO, t1))
        .unwrap();
    assert_eq!(
        ws.run("track!", OnRecovery::Discard).unwrap(),
        ExOutcome::Message("removed track T1".into())
    );
    assert!(ws.current().timeline().track(t1).is_none());
}

/// `:track move` reorders the stack without touching a clip, and refuses to
/// walk off either end rather than quietly doing nothing.
#[test]
fn tracks_can_be_reordered_and_the_move_stops_at_the_ends() {
    let dir = Scratch::new("movetrack");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(
        &mut ws,
        fixture(&[("V1", &[(0, 300, "a")]), ("A1", &[(0, 300, "b")])]),
    );
    let v1 = davimci_core::testing::track_id(ws.current().timeline(), "V1");
    ws.with_session(|s| s.set_playhead(davimci_core::Frame::ZERO, v1))
        .unwrap();

    let err = ws.run("track move up", OnRecovery::Discard).unwrap_err();
    assert_eq!(err.class(), ErrorClass::User);
    assert!(!err.user_message().is_empty());

    ws.run("track move down", OnRecovery::Discard).unwrap();
    let tl = ws.current().timeline();
    assert_eq!(tl.tracks()[1].id, v1);
    assert_eq!(
        tl.track(v1).unwrap().clips()[0].start,
        davimci_core::Frame::ZERO
    );
}

#[test]
fn ungrouping_where_nothing_is_grouped_is_a_user_error() {
    let dir = Scratch::new("ungroup");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    seeded(&mut ws, fixture(&[("V1", &[(0, 300, "a")])]));
    let err = ws.run("ungroup", OnRecovery::Discard).unwrap_err();
    assert_eq!(err.class(), ErrorClass::User);
    assert!(!err.user_message().is_empty());
}

#[test]
fn a_second_import_does_not_pin_the_edited_first_one_clean() {
    let dir = Scratch::new("importquit2");
    let mut ws = Workspace::new(dir.path()).without_autosave();
    ws.import_media(&dir.join("a.mkv"), &StubProber).unwrap();
    let edited = ws.current().id();
    let cmd = split(ws.current().timeline(), 100);
    ws.exec(&cmd).unwrap();

    ws.import_media(&dir.join("b.mkv"), &StubProber).unwrap();
    assert!(!ws.current().is_dirty(), "the new import is its own buffer");

    let i = ws.buffers().iter().position(|b| b.id() == edited).unwrap();
    ws.goto_buffer(i).unwrap();
    assert!(
        ws.current().is_dirty(),
        "an edited buffer stays protected when another import arrives"
    );
}
