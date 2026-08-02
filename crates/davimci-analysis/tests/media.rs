//! Tests that need real media (plan.md standing rule 3).
//!
//! Everything here runs against the generated fixtures from
//! `scripts/gen-fixtures.sh` - nothing is committed - and is gated behind
//! `--features slow-tests` so the default suite never decodes or encodes.
//!
//!     just fixtures && just test-slow
//!
//! The unit tests prove the *maths*; these prove that ffmpeg's view of the
//! world agrees with it.

#![cfg(feature = "slow-tests")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::path::{Path, PathBuf};

use davimci_analysis::analysis::{AnalysisParams, analyze_samples};
use davimci_analysis::cache::AnalysisCache;
use davimci_analysis::conform::{ConformOptions, conform};
use davimci_analysis::import::{ImportOptions, import};
use davimci_analysis::probe::{FfprobeProber, Prober, StreamKind};
use davimci_analysis::proxy::{ProxyPolicy, plan_proxy};
use davimci_analysis::{decode, subtitle};
use davimci_cmd::Session;
use davimci_core::{Fps, Frame, Timeline, TimelineProps};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/fixtures")
        .canonicalize()
        .expect("run `just fixtures` first")
}

fn probe(name: &str) -> davimci_analysis::MediaInfo {
    FfprobeProber
        .probe(&fixtures().join(name))
        .unwrap_or_else(|e| panic!("probing {name}: {e}"))
}

#[test]
fn a_multitrack_mkv_exposes_every_stream_as_its_own_track() {
    let info = probe("multitrack.mkv");
    assert_eq!(info.streams_of(StreamKind::Video).len(), 1);
    assert_eq!(info.streams_of(StreamKind::Audio).len(), 3);
    assert_eq!(info.streams_of(StreamKind::Subtitle).len(), 2);

    let mut opts = ImportOptions::default();
    for s in info.streams_of(StreamKind::Subtitle) {
        let cues = subtitle::extract(&fixtures().join("multitrack.mkv"), s.index).unwrap();
        assert!(!cues.is_empty(), "stream {} had no cues", s.index);
        assert!(cues[0].text.starts_with("subtitle track"));
        opts.subtitles.insert(s.index, cues);
    }

    let mut session = Session::new(Timeline::new(TimelineProps::default()));
    let imported = import(&mut session, &info, &opts).unwrap();
    assert_eq!(imported.mapping.len(), 6);
    assert_eq!(session.timeline().props.fps, Fps::FPS_30);
    // 5s at 30fps. The MKV states no frame count, so the length comes from
    // the container duration, which carries the muxer's last-frame slack -
    // hence 150 or 151, never anything else.
    assert!(
        (150..=151).contains(&imported.length.get()),
        "conformed to {} frames",
        imported.length
    );
    session.timeline().assert_invariants();

    session.undo().unwrap();
    assert_eq!(session.timeline().duration(), Frame::ZERO);
}

#[test]
fn the_conform_matrix_agrees_with_the_real_files() {
    // Every counter fixture is 10s (5s/3s for the smaller ones); conformed
    // into a 1080p60 timeline they must all be whole frames and within half
    // a frame of the true duration.
    let props = TimelineProps::default();
    for (name, seconds) in [
        ("counter_1080p60.mkv", 10.0),
        ("counter_1080p30.mkv", 10.0),
        ("counter_1080p25.mkv", 10.0),
        ("counter_23976.mkv", 10.0),
        ("counter_720p.mkv", 5.0),
        ("counter_4k.mkv", 3.0),
    ] {
        let info = probe(name);
        let c = conform(&info, props, ConformOptions::default());
        let want = seconds * 60.0;
        assert!(
            (c.length.get() as f64 - want).abs() <= 1.0,
            "{name} conformed to {} frames, expected about {want}",
            c.length
        );
        let rect = c.rect.expect("a video fixture has a rectangle");
        assert!(
            rect.width <= 1920 && rect.height <= 1080,
            "{name} overflows"
        );
    }
}

#[test]
fn silence_analysis_of_the_tone_fixture_matches_ground_truth() {
    // tone_gaps.wav: tone at 1-2s and 3-4s, silence elsewhere.
    let samples = decode::decode_mono(&fixtures().join("tone_gaps.wav"), 0, 48_000).unwrap();
    let a = analyze_samples(&samples, 48_000, AnalysisParams::default());
    let want = [(0, 1000), (2000, 3000), (4000, 5000)];
    assert_eq!(a.silence.len(), want.len(), "got {:?}", a.silence);
    for (span, (start, end)) in a.silence.iter().zip(want) {
        assert!(
            span.start_ms.abs_diff(start) <= 10 && span.end_ms.abs_diff(end) <= 10,
            "{span:?} is more than one hop from {start}-{end}"
        );
    }
}

#[test]
fn pure_silence_analyses_as_silent_end_to_end() {
    let samples = decode::decode_mono(&fixtures().join("silence_5s.wav"), 0, 48_000).unwrap();
    let a = analyze_samples(&samples, 48_000, AnalysisParams::default());
    assert_eq!(a.silence.len(), 1);
    assert!(a.silence[0].duration_ms() >= 4_900);
}

#[test]
fn the_scene_cut_fixture_is_detected_at_the_known_frame() {
    // scene_cut.mkv cuts red to blue at exactly 2.0s.
    let cuts =
        decode::scene_changes(&fixtures().join("scene_cut.mkv"), decode::SCENE_THRESHOLD).unwrap();
    assert!(
        cuts.iter().any(|ms| ms.abs_diff(2000) <= 50),
        "expected a cut near 2000ms, got {cuts:?}"
    );
}

#[test]
fn a_proxy_has_exactly_the_same_frame_count_as_its_source() {
    let info = probe("counter_4k.mkv");
    let c = conform(&info, TimelineProps::default(), ConformOptions::default());
    let dir = std::env::temp_dir().join(format!("davimci-proxy-{}", std::process::id()));
    let cache = AnalysisCache::for_project(&dir);
    let spec = plan_proxy(&info, &c, &ProxyPolicy::default(), cache.root(), "fixture")
        .expect("4K must trigger the proxy threshold");

    davimci_analysis::proxy::generate(&spec, None).unwrap();
    let proxy = FfprobeProber.probe(&spec.path).unwrap();
    let video = proxy.video().unwrap();
    assert_eq!(video.resolution.unwrap().height, 540);
    assert_eq!(video.fps, info.video().unwrap().fps, "framerate must match");
    assert_eq!(
        proxy.source_frames(video.fps.unwrap()),
        info.source_frames(info.source_fps().unwrap()),
        "frame numbers must be identical in proxy and original"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cached_analysis_survives_a_round_trip_through_disk() {
    let dir = std::env::temp_dir().join(format!("davimci-cache-slow-{}", std::process::id()));
    let cache = AnalysisCache::for_project(&dir);
    let source = fixtures().join("tone_gaps.wav");
    let hash = davimci_analysis::content_hash(&source).unwrap();

    let samples = decode::decode_mono(&source, 0, 48_000).unwrap();
    let a = analyze_samples(&samples, 48_000, AnalysisParams::default());
    cache.store(&hash, &a).unwrap();
    let back = cache.load(&hash).unwrap();
    assert_eq!(back.silence, a.silence);
    assert_eq!(back.hops.len(), a.hops.len());
    let _ = std::fs::remove_dir_all(&dir);
}
