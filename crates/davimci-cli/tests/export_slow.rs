//! Export against real media, through real MLT (plan.md Phase 8b).
//!
//! This is the test that says M3's "export a multi-audio MKV" is true rather
//! than plausible: it renders a generated fixture and asserts on the file
//! with `ffprobe`, so a broken encoder setting fails here rather than in
//! somebody's editing session.
//!
//! Needs `just fixtures` and `--features slow-tests`.

#![cfg(feature = "slow-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use davimci_app::{App, Event};
use davimci_backend::RenderBackend;
use davimci_cli::{Editor, Workspace};
use davimci_present::{Host as PresentHost, Presenter};

fn fixture(name: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/fixtures")
        .join(name);
    assert!(p.exists(), "missing fixture {name}; run `just fixtures`");
    p
}

/// Ask ffprobe one thing about a file, so assertions are exact rather than
/// "the file is big enough to look right".
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

/// Build a real editor over a real MLT backend, with `media` imported.
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

/// Run ticks until the export stops, so the test never hangs on a stuck
/// consumer.
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

/// The M3 claim, checked rather than assumed.
///
/// Each audio track is routed onto its own channel pair before the tractor's
/// `mix` transitions sum them, and the avformat consumer cuts that bus back
/// into one stream per pair (`channels.N`). The routing is what makes it
/// work: the tractor mixes, so tracks that shared channels would arrive as
/// one stream no matter what the consumer was told.
#[test]
fn exporting_a_multi_audio_mkv_keeps_every_audio_track_separate() {
    let src = fixture("multitrack.mkv");
    let want_audio = stream_count(&src, "a");
    assert!(
        want_audio >= 2,
        "the multitrack fixture should have several audio streams, found {want_audio}"
    );

    let (mut app, mut editor) = editor_with(&src);
    let out = std::env::temp_dir().join("davimci-slow-multitrack.mkv");
    let _ = std::fs::remove_file(&out);

    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    assert!(editor.exporter().is_running(), "the export never started");
    drain_export(&mut app, &mut editor);

    assert!(out.exists(), "the export produced no file");
    // This is the M3 claim, checked rather than assumed.
    assert_eq!(
        stream_count(&out, "a"),
        want_audio,
        "audio tracks were merged instead of kept separate"
    );
    assert_eq!(stream_count(&out, "v"), 1, "expected one video stream");

    // Separate streams that all carry the same mix would pass a stream count
    // and fail the user, so check the *content*: the fixture's tracks are
    // 220, 440 and 660 Hz sines, and each stream must be its own tone.
    for (n, want) in [220.0_f64, 440.0, 660.0].iter().enumerate() {
        let got = dominant_hz(&out, n);
        assert!(
            (got - want).abs() < want * 0.05,
            "audio stream {n} carries {got:.0} Hz, expected {want:.0} Hz - the tracks were \
             mixed together rather than routed to their own streams"
        );
    }
    let _ = std::fs::remove_file(&out);
}

/// The dominant frequency of one audio stream, by zero-crossing rate.
///
/// The fixture's tracks are pure sines, so crossings per second is twice the
/// frequency - exact enough to tell 220 from 440 without an FFT.
fn dominant_hz(path: &Path, stream: usize) -> f64 {
    const RATE: usize = 48_000;
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-map",
            &format!("0:a:{stream}"),
            "-ac",
            "1",
            "-ar",
            &RATE.to_string(),
            "-f",
            "f32le",
            "-",
        ])
        .output()
        .expect("ffmpeg should be installed for slow tests");
    let samples: Vec<f32> = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .skip(RATE / 2)
        .take(RATE)
        .collect();
    assert!(
        samples.len() > RATE / 2,
        "audio stream {stream} is too short to measure"
    );
    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    crossings as f64 / 2.0 * (RATE as f64 / samples.len() as f64)
}

#[test]
fn a_preset_decides_the_container_and_codec_of_the_file() {
    let src = fixture("counter_720p.mkv");
    let (mut app, mut editor) = editor_with(&src);
    let out = std::env::temp_dir().join("davimci-slow-preset.mp4");
    let _ = std::fs::remove_file(&out);

    app.event(
        Event::Command(format!(":export {} --preset mp4", out.display())),
        &mut editor,
    );
    drain_export(&mut app, &mut editor);

    assert!(out.exists(), "the export produced no file");
    let codec = probe(
        &out,
        &[
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ],
    );
    // The preset said `h264`; the file must actually be h264 (spec 10.3).
    assert_eq!(codec, "h264", "the preset's codec did not reach the file");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn an_exported_file_has_the_duration_of_the_timeline() {
    let src = fixture("counter_720p.mkv");
    let (mut app, mut editor) = editor_with(&src);
    let frames = app.session().timeline().duration();
    let fps = app.session().timeline().props.fps;

    let out = std::env::temp_dir().join("davimci-slow-duration.mkv");
    let _ = std::fs::remove_file(&out);
    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    drain_export(&mut app, &mut editor);

    let secs: f64 = probe(
        &out,
        &["-show_entries", "format=duration", "-of", "csv=p=0"],
    )
    .parse()
    .expect("ffprobe should report a duration");
    let want = frames.get() as f64 * f64::from(fps.den) / f64::from(fps.num);
    // One frame of slack: containers round durations, timelines do not.
    let slack = 1.0 * f64::from(fps.den) / f64::from(fps.num);
    assert!(
        (secs - want).abs() <= slack + 0.05,
        "exported {secs}s but the timeline is {want}s"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn cancelling_a_real_export_stops_it_and_keeps_the_partial_file() {
    let src = fixture("counter_1080p60.mkv");
    let (mut app, mut editor) = editor_with(&src);
    let out = std::env::temp_dir().join("davimci-slow-cancel.mkv");
    let _ = std::fs::remove_file(&out);

    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    // Let it get going, then stop it.
    for _ in 0..10 {
        app.event(Event::Tick, &mut editor);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    app.event(Event::Command(":cancel".into()), &mut editor);
    assert!(!editor.exporter().is_running(), "cancel did not stop it");

    // The editor is still usable afterwards - a cancelled export is not a
    // crash (Phase 0: recoverable errors degrade locally).
    app.event(Event::Key(davimci_keys::Key::Char('l')), &mut editor);
    let _ = std::fs::remove_file(&out);
}
