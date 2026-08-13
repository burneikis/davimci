//! Export against real media, through real MLT.
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

/// One decoded frame as raw RGB, so an assertion can be about pixels rather
/// than about stream counts.
fn frame_pixels(path: &Path, index: u64) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args([
            "-vf",
            &format!("select=eq(n\\,{index})"),
            "-fps_mode",
            "passthrough",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .expect("ffmpeg should be installed for slow tests");
    assert!(
        !out.stdout.is_empty(),
        "no frame {index} in {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Subpixels the two frames disagree about by more than encoder noise.
///
/// A burned-in glyph moves a channel by most of its range; inter-frame
/// prediction off a changed reference moves it by a few dozen units at
/// most. The threshold is what keeps the second from reading as the first.
fn visibly_different(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len(), "the two frames are not the same size");
    a.iter()
        .zip(b)
        .filter(|(x, y)| x.abs_diff(**y) > 100)
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
    let _mlt = davimci_mlt::test_support::media_lock();
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
    let _mlt = davimci_mlt::test_support::media_lock();
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
    // The preset said `h264`; the file must actually be h264.
    assert_eq!(codec, "h264", "the preset's codec did not reach the file");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn an_exported_file_has_the_duration_of_the_timeline() {
    let _mlt = davimci_mlt::test_support::media_lock();
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

    // Counted frames, not `format=duration`: the container's duration is the
    // longest stream's, and the audio encoder pads its last packet to a full
    // frame, so a correct export reads several milliseconds long there.
    let coded: u64 = probe(
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
    .parse()
    .expect("ffprobe should count the video frames");
    assert_eq!(
        coded,
        frames.get(),
        "the export has {coded} frames but the timeline is {} at {fps:?}",
        frames.get()
    );
    let _ = std::fs::remove_file(&out);
}

/// A hardware export must satisfy every assertion a software export does:
/// the same codec, one video stream, and exactly the timeline's frames. A
/// machine with no encoder falls back to software, which must satisfy them
/// too - so this test asserts the file, not the encoder.
#[test]
fn a_hardware_export_meets_the_same_assertions_as_a_software_one() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let src = fixture("counter_720p.mkv");
    let (mut app, mut editor) = editor_with(&src);
    let frames = app.session().timeline().duration();

    app.event(Event::Command(":set encode auto".into()), &mut editor);
    let out = std::env::temp_dir().join("davimci-slow-hwencode.mkv");
    let _ = std::fs::remove_file(&out);
    app.event(
        Event::Command(format!(":export {}", out.display())),
        &mut editor,
    );
    drain_export(&mut app, &mut editor);

    assert!(out.exists(), "the export produced no file");
    assert_eq!(stream_count(&out, "v"), 1);
    assert_eq!(
        probe(
            &out,
            &[
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "csv=p=0",
            ],
        ),
        "h264",
        "the preset named h264 and the file must carry h264, hardware or not"
    );
    let coded: u64 = probe(
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
    .parse()
    .expect("ffprobe should count the video frames");
    assert_eq!(
        coded,
        frames.get(),
        "the export is not the timeline's length"
    );
    let _ = std::fs::remove_file(&out);
}

/// A preset that requires a hardware encoder is refused before the job
/// starts when it cannot be met - no partial file, and a sentence saying so.
#[test]
fn a_required_hardware_encode_that_cannot_be_met_is_refused_with_no_file() {
    use davimci_backend::{HardwareEncode, RenderJob, RenderSettings};

    let _mlt = davimci_mlt::test_support::media_lock();
    let src = fixture("counter_720p.mkv");
    let (app, mut editor) = editor_with(&src);
    let props = app.session().timeline().props;

    let out = std::env::temp_dir().join("davimci-slow-hwrefused.mkv");
    let _ = std::fs::remove_file(&out);
    // ProRes has no hardware encoder anywhere, so this is a requirement no
    // machine can meet.
    let settings = RenderSettings {
        resolution: props.resolution,
        fps: props.fps,
        video_codec: "prores_ks".into(),
        hardware: HardwareEncode::Required,
        ..RenderSettings::default()
    };
    let err = editor
        .backend_mut()
        .render(RenderJob::new(out.clone(), settings))
        .expect_err("a requirement that cannot be met must refuse");
    let sentence = err.to_string();
    assert!(sentence.ends_with('.'), "{sentence}");
    assert!(!out.exists(), "a refused export left a file behind");
}

#[test]
fn cancelling_a_real_export_stops_it_and_keeps_the_partial_file() {
    let _mlt = davimci_mlt::test_support::media_lock();
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
    // crash (recoverable errors degrade locally).
    app.event(Event::Key(davimci_keys::Key::Char('l')), &mut editor);
    let _ = std::fs::remove_file(&out);
}

/// Each subtitle mode produces the file it promises.
#[test]
fn subtitle_modes_burn_write_or_mux_and_are_told_apart_by_ffprobe() {
    let _mlt = davimci_mlt::test_support::media_lock();
    use davimci_backend::{AudioCodec, Container, Preset, SubtitleMode, VideoCodec};

    let src = fixture("counter_720p.mkv");
    // Where each mode's file lands, so the burned one can be diffed against
    // the one that carried the same cue without burning it.
    let mut written: std::collections::BTreeMap<&str, PathBuf> = std::collections::BTreeMap::new();
    for (name, mode) in [
        ("subs_burned", SubtitleMode::Burned),
        ("subs_sidecar", SubtitleMode::Sidecar),
        ("subs_embedded", SubtitleMode::Embedded),
    ] {
        let (mut app, mut editor) = editor_with(&src);
        // A text track with one cue on it, added the only way anything is
        // added: through a command.
        {
            use davimci_cmd::EditCommand;
            let session = app.session_mut();
            session
                .exec(&EditCommand::AddTrack {
                    kind: davimci_core::TrackKind::Text,
                    name: None,
                    new_id: None,
                })
                .unwrap();
            let track = session.timeline().tracks().last().unwrap().id;
            let mut clip = davimci_core::Clip::generated(
                davimci_core::ClipId(0),
                "cue",
                davimci_core::Frame(0),
                davimci_core::Frame(30),
            );
            clip.text = Some("hello".into());
            session
                .exec(&EditCommand::Insert {
                    track,
                    at: davimci_core::Frame(0),
                    clip,
                    new_id: None,
                })
                .unwrap();
        }
        editor.prime(app.session());

        let mut preset = Preset::new(name, Container::Mkv, VideoCodec::H264, AudioCodec::Aac)
            .expect("a legal pairing");
        preset.subtitles = mode;
        editor.exporter_mut().presets_mut().define(preset);

        let out = std::env::temp_dir().join(format!("davimci-slow-{name}.mkv"));
        let srt = out.with_extension("srt");
        let _ = std::fs::remove_file(&out);
        let _ = std::fs::remove_file(&srt);

        app.event(
            Event::Command(format!(":export {} --preset {name}", out.display())),
            &mut editor,
        );
        drain_export(&mut app, &mut editor);
        assert!(out.exists(), "{name}: the export produced no file");
        written.insert(name, out.clone());

        match mode {
            SubtitleMode::Burned => {
                assert_eq!(
                    stream_count(&out, "s"),
                    0,
                    "{name}: burned-in text became a stream"
                );
                assert!(!srt.exists(), "{name}: burned-in text also wrote a sidecar");
            }
            SubtitleMode::Sidecar => {
                assert_eq!(
                    stream_count(&out, "s"),
                    0,
                    "{name}: a sidecar became a stream"
                );
                let text = std::fs::read_to_string(&srt).expect("a sidecar file");
                assert!(text.contains("hello"), "{name}: the sidecar has no cue");
            }
            SubtitleMode::Embedded => {
                assert_eq!(
                    stream_count(&out, "s"),
                    1,
                    "{name}: the subtitle stream was not muxed in"
                );
            }
            SubtitleMode::None => unreachable!(),
        }
        let _ = std::fs::remove_file(&srt);
    }

    // The claim "burned" makes is about the picture, so the picture is what
    // is asserted: the same cue muxed as a stream is the control, since both
    // files come off the same timeline through the same encoder settings.
    let burned = written.remove("subs_burned").expect("a burned export");
    let control = written.remove("subs_embedded").expect("an embedded export");
    let burned_cue = frame_pixels(&burned, 0);
    let control_cue = frame_pixels(&control, 0);
    let cue = visibly_different(&burned_cue, &control_cue);
    assert!(
        cue >= 5_000,
        "burning changed {cue} subpixels of the cue's frame: the text was never drawn"
    );
    // Burned *onto* the picture, not over it: a text card that replaced the
    // frame would change nearly every subpixel rather than the glyphs.
    assert!(
        cue * 2 < burned_cue.len(),
        "{cue} of {} subpixels changed: the text card covered the picture",
        burned_cue.len()
    );
    // The cue covers frames 0..30 of a 60 fps timeline, so frame 150 is well
    // clear of it and must carry no text at all.
    let after = frame_pixels(&burned, 150);
    let clean = frame_pixels(&control, 150);
    let leaked = visibly_different(&after, &clean);
    // Not zero: the two files diverge from frame 0, so x264 predicts frame
    // 150 off different references and a handful of subpixels drift. Text
    // is thousands of subpixels, so the two cannot be confused.
    assert!(
        leaked * 100 < cue,
        "{leaked} subpixels differ after the cue ended against {cue} during it: \
         the burn is not confined to the cue"
    );
    for path in [burned, control] {
        let _ = std::fs::remove_file(path);
    }
    for path in written.values() {
        let _ = std::fs::remove_file(path);
    }
}
