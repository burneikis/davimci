//! The whole editing workflow, start to finish, through the real editor.
//!
//! Import a multi-track MKV, cut sections out, quieten and trim an audio
//! track, layer an overlay, add a subtitle, export. Every step goes through
//! the same path a user's keystroke does - keys, `:` lines and commands - so
//! this fails if any layer between the keymap and the encoder breaks, which
//! is what an integration test is for.
//!
//! Needs `just fixtures` and `--features slow-tests`.

#![cfg(feature = "slow-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use davimci_app::{App, Event};
use davimci_backend::RenderBackend;
use davimci_cli::{Editor, Workspace};
use davimci_cmd::EditCommand;
use davimci_core::{Clip, ClipId, Edge, Frame, TrackKind};
use davimci_keys::Key;
use davimci_present::{Host as PresentHost, Presenter};

fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/fixtures")
        .join(name);
    assert!(p.exists(), "missing fixture {name}; run `just fixtures`");
    p
}

fn probe(path: &Path, args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .expect("ffprobe should be installed for slow tests");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stream_count(path: &Path, kind: &str) -> usize {
    probe(
        path,
        &[
            "-select_streams",
            kind,
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ],
    )
    .lines()
    .filter(|l| !l.trim().is_empty())
    .count()
}

fn editor_with(media: &Path) -> (App, Editor) {
    let mut ws = Workspace::new(std::env::temp_dir()).without_autosave();
    ws.import_media(media, &davimci_analysis::FfprobeProber)
        .expect("the fixture should import");
    let session = ws.current_session();
    let props = session.timeline().props;
    let backend: Box<dyn RenderBackend> =
        Box::new(davimci_mlt::MltBackend::new(props).expect("libmlt should load"));
    let presenter = Presenter::new(PresentHost::Embedded, props.resolution, props.fps);
    let mut editor = Editor::new(ws, backend, presenter);
    let app = App::new(session);
    editor.prime(app.session());
    (app, editor)
}

fn keys(app: &mut App, editor: &mut Editor, script: &str) {
    for key in Key::parse_str(script) {
        app.event(Event::Key(key), editor);
    }
}

fn drain_export(app: &mut App, editor: &mut Editor) {
    for _ in 0..20_000 {
        app.event(Event::Tick, editor);
        if !editor.exporter().is_running() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the export never finished");
}

#[test]
fn the_whole_spec_one_workflow_survives_a_real_import_and_export() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let src = fixture("multitrack.mkv");
    let audio_streams = stream_count(&src, "a");
    assert!(audio_streams >= 2, "the fixture should be multi-track");

    let (mut app, mut editor) = editor_with(&src);
    let duration_before = app.session().timeline().duration();
    assert!(duration_before.get() > 0, "the import produced no footage");
    // The import is itself an edit, so the state to come back to is a node in
    // the undo tree rather than the tree's root.
    let imported = app.session().history().current();

    // 2. Cut a section out: split twice on the video track and ripple delete
    //    what is between the cuts, exactly as a user types it.
    keys(&mut app, &mut editor, "gg");
    let v1 = app.session().timeline().tracks()[0].id;
    app.session_mut()
        .set_playhead(Frame(30), v1)
        .expect("the playhead is inside the footage");
    keys(&mut app, &mut editor, "sl");
    app.session_mut().set_playhead(Frame(60), v1).unwrap();
    keys(&mut app, &mut editor, "s");
    app.session_mut().set_playhead(Frame(30), v1).unwrap();
    keys(&mut app, &mut editor, "x");
    let v1_after = app.session().timeline().track(v1).unwrap().duration();
    assert!(
        v1_after < duration_before,
        "the ripple delete did not shorten the video track"
    );

    // 3. Quieten and trim an audio track: mute it, then trim its head.
    let a1 = app
        .session()
        .timeline()
        .tracks()
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id)
        .expect("the fixture has audio");
    let first_audio_clip = app.session().timeline().track(a1).unwrap().clips()[0].id;
    app.session_mut().set_playhead(Frame(0), a1).unwrap();
    keys(&mut app, &mut editor, " m");
    assert!(
        app.session().timeline().track(a1).unwrap().muted,
        "<Space>m did not mute the track"
    );
    app.session_mut()
        .exec(&EditCommand::Trim {
            track: a1,
            clip: first_audio_clip,
            edge: Edge::Tail,
            delta: -10,
        })
        .expect("trimming inside the clip is legal");
    app.event(Event::Command(":gain -6".to_string()), &mut editor);

    // 4. Layer an overlay on top.
    app.session_mut()
        .exec(&EditCommand::AddTrack {
            kind: TrackKind::Overlay,
            name: None,
            new_id: None,
        })
        .unwrap();
    let overlay = app.session().timeline().tracks().last().unwrap().id;
    app.session_mut()
        .exec(&EditCommand::Insert {
            track: overlay,
            at: Frame(0),
            clip: Clip::generated(ClipId(0), "badge", Frame(0), Frame(20)),
            new_id: None,
        })
        .unwrap();
    app.session_mut().set_playhead(Frame(0), overlay).unwrap();
    app.event(
        Event::Command(":set clip.scale 0.5".to_string()),
        &mut editor,
    );

    // 5. Add a subtitle, through the `:` lines a user types rather than by
    //    building the commands here.
    app.event(Event::Command(":track text".to_string()), &mut editor);
    let text = app.session().timeline().tracks().last().unwrap().id;
    app.session_mut().set_playhead(Frame(0), text).unwrap();
    app.event(Event::Command(":subtitle hello".to_string()), &mut editor);
    assert_eq!(
        app.session().timeline().track(text).unwrap().clips()[0]
            .text
            .as_deref(),
        Some("hello")
    );

    // The golden snapshot: the shape of the timeline the workflow built. It
    // is compared as text so a structural regression reads as a diff rather
    // than as a failed count.
    let dump = app.session().timeline().dump();
    let shape: Vec<String> = dump
        .lines()
        .map(|l| {
            let (name, rest) = l.split_once(':').unwrap_or((l, ""));
            format!("{name} {} clip(s)", rest.matches('[').count())
        })
        .collect();
    assert_eq!(
        shape,
        vec![
            "V1 2 clip(s)".to_string(),
            // Import groups the streams of one file together, so the split
            // and the ripple delete on V1 land on the audio that came in
            // with it. Two clips per audio track is the group working; one
            // would mean the picture and its sound had drifted apart.
            "A1 2 clip(s)".to_string(),
            "A2 2 clip(s)".to_string(),
            "A3 2 clip(s)".to_string(),
            // The fixture carries two subtitle streams of its own, each
            // with one cue, which the import reads; the cue this workflow
            // types lands on a third text track.
            "T1 1 clip(s)".to_string(),
            "T2 1 clip(s)".to_string(),
            "O1 1 clip(s)".to_string(),
            "T3 1 clip(s)".to_string(),
        ],
        "the workflow built a different timeline:\n{dump}"
    );
    app.session().timeline().check_invariants().unwrap();

    // 6. Export, and let ffprobe say whether it worked.
    editor.prime(app.session());
    let out = std::env::temp_dir().join("davimci-slow-workflow.mkv");
    let _ = std::fs::remove_file(&out);
    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    assert!(editor.exporter().is_running(), "the export never started");
    drain_export(&mut app, &mut editor);

    assert!(out.exists(), "the export produced no file");
    assert_eq!(stream_count(&out, "v"), 1, "expected one video stream");
    assert_eq!(
        stream_count(&out, "a"),
        audio_streams,
        "the audio tracks were merged"
    );
    let frames = probe(
        &out,
        &[
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "csv=p=0",
        ],
    )
    .parse::<u64>()
    .expect("ffprobe should count frames");
    // The picture, not the timeline: a plain `:export` carries no subtitles,
    // so a text track is not part of what gets rendered and a cue sitting
    // past the last frame of video does not lengthen the file. The ripple
    // delete shortened the picture and left the loose cues where they were,
    // which is exactly the case that tells the two lengths apart.
    let picture = app
        .session()
        .timeline()
        .tracks()
        .iter()
        .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio))
        .map(davimci_core::Track::duration)
        .max()
        .unwrap();
    assert_eq!(
        frames,
        picture.get(),
        "the export is not as long as the picture"
    );
    assert!(
        app.session().timeline().duration() > picture,
        "the imported cues should outlast the shortened picture"
    );

    // And every edit still undoes, after a real export.
    app.session_mut()
        .goto(imported)
        .expect("the imported state is still in the tree");
    assert_eq!(app.session().timeline().duration(), duration_before);
    assert_eq!(app.session().timeline().track(v1).unwrap().clips().len(), 1);
    app.session().timeline().check_invariants().unwrap();

    let _ = std::fs::remove_file(&out);
}

/// Importing a container with subtitle streams brings the cues in, not just
/// the empty tracks: a caption written elsewhere has to survive the import
/// or the text track is a promise the editor does not keep.
///
/// Regression: `:e <media>` and the media picker both built their
/// `ImportOptions` without ever calling `subtitle::extract`, so every
/// imported subtitle stream produced a track with nothing on it.
#[test]
fn importing_a_container_with_subtitles_brings_the_cues_with_it() {
    let mut ws = Workspace::new(std::env::temp_dir()).without_autosave();
    ws.import_media(&fixture("multitrack.mkv"), &davimci_analysis::FfprobeProber)
        .expect("the fixture should import");
    let tl = ws.current().timeline();

    let text: Vec<_> = tl
        .tracks()
        .iter()
        .filter(|t| t.kind == TrackKind::Text)
        .collect();
    assert_eq!(text.len(), 2, "the fixture has two subtitle streams");
    for track in text {
        let clips = track.clips();
        assert_eq!(clips.len(), 1, "{} came in with no cues", track.name);
        // `scripts/gen-fixtures.sh` writes one cue per stream, and its text
        // names the stream it belongs to.
        assert!(
            clips[0]
                .text
                .as_deref()
                .is_some_and(|t| t.starts_with("subtitle track")),
            "{} carried {:?}",
            track.name,
            clips[0].text
        );
        assert!(clips[0].media.is_none(), "a cue is a generated clip");
    }
    tl.check_invariants().unwrap();
}
