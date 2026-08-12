//! Backend tests that decode or encode real media.
//!
//!     just fixtures && just test-slow
//!
//! The fast suite proves the graph is built and patched correctly; these
//! prove that what comes out of it is the frame that was asked for.

#![cfg(feature = "slow-tests")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use davimci_backend::{
    PreviewScale, RenderBackend, RenderJob, RenderSettings, RenderState, VideoFrame,
};
use davimci_core::{Clip, ClipId, Fps, Frame, MediaRef, Resolution, Timeline, TimelineProps};
use davimci_mlt::MltBackend;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/fixtures")
        .canonicalize()
        .expect("run `just fixtures` first")
}

/// A 640x480@60 timeline holding one fixture file end to end.
fn timeline_of(name: &str, frames: u64, res: Resolution) -> Timeline {
    let mut tl = Timeline::new(TimelineProps {
        fps: Fps::FPS_60,
        resolution: res,
        sample_rate: 48_000,
    });
    let v1 = tl.track_by_name("V1").map(|t| t.id).unwrap();
    let id = tl.new_clip_id();
    let media = MediaRef::new(
        fixtures().join(name).to_string_lossy().to_string(),
        Fps::FPS_60,
        Frame(frames),
    );
    let clip = Clip::from_media(
        id,
        "fixture",
        media,
        Frame::ZERO,
        Frame::ZERO,
        Frame(frames),
    );
    tl.restore(v1, Frame::ZERO, &[clip], Frame(frames), false)
        .unwrap();
    tl
}

/// Dominant channel of a frame: 0 red, 2 blue.
fn dominant(f: &VideoFrame) -> usize {
    let s = f.signature();
    let mut best = 0;
    for c in 1..3 {
        if s[c] > s[best] {
            best = c;
        }
    }
    best
}

/// `scene_cut.mkv` is red for frames 0-119 and blue for 120-239 at 60fps, so
/// the colour *is* the frame number's signature - no OCR needed.
#[test]
fn seek_lands_on_the_exact_frame() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let before = b.frame_at(Frame(119), PreviewScale::Full).unwrap();
    let after = b.frame_at(Frame(120), PreviewScale::Full).unwrap();
    assert_eq!(dominant(&before), 0, "frame 119 is the last red frame");
    assert_eq!(dominant(&after), 2, "frame 120 is the first blue frame");

    // Seeking backwards must be just as exact as seeking forwards.
    let again = b.frame_at(Frame(10), PreviewScale::Full).unwrap();
    assert_eq!(dominant(&again), 0);
}

#[test]
fn a_quarter_res_pull_is_the_same_frame_scaled() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let full = b.frame_at(Frame(200), PreviewScale::Full).unwrap();
    let quarter = b.frame_at(Frame(200), PreviewScale::Quarter).unwrap();
    assert_eq!(quarter.width, 160);
    assert_eq!(quarter.height, 120);
    assert_eq!(
        dominant(&full),
        dominant(&quarter),
        "a scaled pull must be the same frame, never a different one"
    );
}

#[test]
fn consecutive_pulls_are_monotonic_and_never_duplicate() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let mut last: Option<Frame> = None;
    for n in 100..110 {
        let f = b.frame_at(Frame(n), PreviewScale::Half).unwrap();
        assert_eq!(f.position, Frame(n));
        if let Some(prev) = last {
            assert!(f.position > prev);
        }
        last = Some(f.position);
    }
}

/// Stepping backwards used to re-seek per frame, and a seek re-decodes from
/// the preceding keyframe - so walking back through a GOP cost a GOP per
/// frame. The run-ahead makes it one decode per frame, exactly as forwards.
#[test]
fn stepping_backwards_decodes_each_frame_once_and_is_exact() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    // Land on 130 (blue), then walk back over the cut into red.
    let start = 130u64;
    b.frame_at(Frame(start), PreviewScale::Half).unwrap();
    let baseline = b.decodes;

    let steps = 24u64;
    for n in (start - steps..start).rev() {
        let f = b.frame_at(Frame(n), PreviewScale::Half).unwrap();
        assert_eq!(f.position, Frame(n), "a backward step returned frame {n:?}");
        let want = if n >= 120 { 2 } else { 0 };
        assert_eq!(
            dominant(&f),
            want,
            "frame {n} came back with the wrong picture"
        );
    }
    let decoded = b.decodes - baseline;
    assert!(
        decoded <= steps as usize + b.backstep_run as usize,
        "{steps} backward steps decoded {decoded} frames; the cache is not being hit"
    );
    assert!(
        b.cache_hits >= steps as usize - (steps as usize).div_ceil(b.backstep_run as usize),
        "backward steps never hit the cache"
    );
}

/// Regression: the prefetcher asked for the run below the last one it
/// decoded on every step, so it advanced a run for every frame the user
/// stepped - racing the walk to frame 0, decoding the whole timeline and
/// evicting the pictures the walk was about to ask for. It must stay about
/// one run ahead, and every picture it delivers must be the frame it claims.
#[test]
fn the_prefetcher_stays_one_run_ahead_and_delivers_the_right_pictures() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let run = b.backstep_run;
    let start = 160u64;
    let steps = 40u64;
    b.frame_at(Frame(start), PreviewScale::Half).unwrap();
    for n in (start - steps..start).rev() {
        // Slower than the worker, which is the case that used to run away.
        std::thread::sleep(Duration::from_millis(20));
        let f = b.frame_at(Frame(n), PreviewScale::Half).unwrap();
        assert_eq!(f.position, Frame(n));
        // `scene_cut.mkv` is red below frame 120 and blue from there up, so
        // a picture from the wrong side of the cut shows in one channel.
        let want = usize::from(n >= 120) * 2;
        assert_eq!(
            dominant(&f),
            want,
            "frame {n} came back with another frame's picture"
        );
    }
    assert!(
        b.prefetched <= (steps + 2 * run) as usize,
        "the worker decoded {} frames for a {steps}-frame walk: it is racing ahead",
        b.prefetched
    );
}

/// Regression: the first backward step of every run used to decode that run
/// on the caller's thread, so a held `h` hitched once every `backstep_run`
/// frames. The run below what is cached is decoded by a worker with a graph
/// of its own while the transport is idle, so the walk arrives to find the
/// pictures already there.
#[test]
fn a_backward_walk_finds_the_next_run_already_decoded() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let run = b.backstep_run;
    let start = 200u64;
    b.frame_at(Frame(start), PreviewScale::Half).unwrap();
    // The first backward step decodes its own run inline and asks the
    // worker for the one below it.
    b.frame_at(Frame(start - 1), PreviewScale::Half).unwrap();

    // The worker has a whole run to decode; give it longer than it needs
    // before asserting that it did.
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        b.frame_at(Frame(start - 2), PreviewScale::Half).unwrap();
        if b.prefetched >= run as usize {
            break;
        }
    }
    assert!(
        b.prefetched >= run as usize,
        "the worker delivered {} of {run} frames: the run below is still decoded inline",
        b.prefetched
    );

    // Walking off the bottom of the first run is served from what the
    // worker brought, so every step is a cache hit.
    let hits = b.cache_hits;
    let steps = run + 2;
    for n in (start - 2 - steps..start - 2).rev() {
        let f = b.frame_at(Frame(n), PreviewScale::Half).unwrap();
        assert_eq!(f.position, Frame(n));
    }
    assert_eq!(
        b.cache_hits - hits,
        steps as usize,
        "a step off the bottom of the run still decoded on the caller's thread"
    );
}

/// Regression: a quarter-scale thumbnail pulled between two preview steps
/// used to clear the frame cache and stand in for the playhead, so the next
/// backward step both missed and was taken for a forward one - a GOP per
/// frame, with the picture hitching whenever a strip was filling in.
#[test]
fn a_thumbnail_between_steps_costs_neither_the_cache_nor_the_backstep() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let start = 130u64;
    b.frame_at(Frame(start), PreviewScale::Half).unwrap();
    // The first backward step decodes the run leading up to it.
    b.frame_at(Frame(start - 1), PreviewScale::Half).unwrap();
    let baseline = b.decodes;

    // A thumbnail far away, at the scale the timeline strips use.
    b.thumbnail_at(Frame(0), PreviewScale::Quarter).unwrap();
    let hits = b.cache_hits;

    // The run is still there, and the step after it is still a backward one.
    b.frame_at(Frame(start - 2), PreviewScale::Half).unwrap();
    assert_eq!(
        b.cache_hits,
        hits + 1,
        "the thumbnail evicted the preview run"
    );

    // Walk off the bottom of the run: one run, not one GOP per frame.
    let steps = 24u64;
    for n in (start - steps..start - 2).rev() {
        let f = b.frame_at(Frame(n), PreviewScale::Half).unwrap();
        assert_eq!(f.position, Frame(n));
    }
    let decoded = b.decodes - baseline;
    assert!(
        decoded <= steps as usize + 2 * b.backstep_run as usize,
        "{steps} steps around a thumbnail decoded {decoded} frames"
    );
}

/// A cached still of an edited timeline is a picture of a timeline that no
/// longer exists, so any graph change must throw the cache away.
#[test]
fn editing_the_timeline_invalidates_cached_stills() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();
    b.frame_at(Frame(200), PreviewScale::Half).unwrap();
    let hits = b.cache_hits;
    assert_eq!(
        b.frame_at(Frame(200), PreviewScale::Half).map(|_| ()),
        Ok(()),
    );
    assert_eq!(b.cache_hits, hits + 1, "the still was not cached at all");

    // A different source entirely: the frame at 200 is now another picture.
    let other = timeline_of("sync_flash.mkv", 240, res);
    b.set_timeline(&other).unwrap();
    let hits = b.cache_hits;

    b.frame_at(Frame(200), PreviewScale::Half).unwrap();
    assert_eq!(b.cache_hits, hits, "a stale still survived an edit");
}

/// How far a hardware-decoded frame's mean channel may sit from the
/// software-decoded one.
///
/// A separate, named tolerance for the hardware path only: VAAPI's colour
/// conversion is not required to be bit-exact with swscale, so the software
/// path stays the reference and keeps its exact assertions. Never apply this
/// to a CPU-path comparison.
const HARDWARE_DECODE_TOLERANCE: i32 = 4;

/// A 1080p h264 fixture is exactly what phase 1 targets: long-GOP, above the
/// readback threshold. Without a render device the test asserts the other
/// half of the contract - that the session silently keeps decoding in
/// software.
#[test]
fn hardware_decode_matches_software_decode_or_falls_back_to_it() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 1920,
        height: 1080,
    };
    let tl = timeline_of("counter_1080p60.mkv", 600, res);

    let mut cpu = MltBackend::new(tl.props).unwrap();
    cpu.set_timeline(&tl).unwrap();
    let software: Vec<[u8; 4]> = [0u64, 90, 300]
        .iter()
        .map(|f| {
            cpu.frame_at(Frame(*f), PreviewScale::Full)
                .unwrap()
                .signature()
        })
        .collect();

    let mut hw = MltBackend::new(tl.props).unwrap();
    let status = hw.set_decode_policy(davimci_backend::DecodePolicy::Auto);
    hw.set_timeline(&tl).unwrap();
    let hardware: Vec<[u8; 4]> = [0u64, 90, 300]
        .iter()
        .map(|f| {
            hw.frame_at(Frame(*f), PreviewScale::Full)
                .unwrap()
                .signature()
        })
        .collect();

    if status.is_hardware() {
        assert!(
            hw.hardware_producers() > 0,
            "a 1080p h264 source was not handed to the hardware decoder"
        );
    } else {
        assert_eq!(hw.hardware_producers(), 0);
        assert!(status.detail.ends_with('.'), "{}", status.detail);
    }

    for (want, got) in software.iter().zip(&hardware) {
        for c in 0..4 {
            let delta = i32::from(want[c]) - i32::from(got[c]);
            assert!(
                delta.abs() <= HARDWARE_DECODE_TOLERANCE,
                "hardware decode disagreed with software: {want:?} vs {got:?}"
            );
        }
    }
}

/// A planar pull is the same picture as the RGBA pull, in three eighths of
/// the bytes.
///
/// Not bit-exact: MLT converts to RGBA with swscale and this converts with
/// the BT.709 matrix in `davimci-backend`, so the comparison is the mean
/// channel under a tolerance that belongs to this path alone. RGBA remains
/// the reference the golden tests assert against, and this tolerance is
/// never applied to them.
const PLANAR_TOLERANCE: i32 = 12;

#[test]
fn a_planar_pull_is_the_same_picture_in_fewer_bytes() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();
    assert!(b.supports_planar());

    for at in [Frame(30), Frame(200)] {
        let rgba = b.frame_at(at, PreviewScale::Full).unwrap();
        let planar = b.planar_frame_at(at, PreviewScale::Full).unwrap();
        assert!(planar.is_well_formed());
        assert_eq!((planar.width, planar.height), (rgba.width, rgba.height));
        assert_eq!(
            planar.bytes() * 8,
            rgba.rgba.len() * 3,
            "a planar frame must be three eighths of the RGBA upload"
        );
        let want = rgba.signature();
        let got = planar.to_rgba().signature();
        for c in 0..3 {
            assert!(
                (i32::from(want[c]) - i32::from(got[c])).abs() <= PLANAR_TOLERANCE,
                "planar decode is a different picture: {want:?} vs {got:?}"
            );
        }
    }
}

/// Phase 1's before/after number, printed rather than asserted: a wall clock
/// is not a correctness claim, and a machine without a device has nothing to
/// compare.
#[test]
fn decode_cost_per_frame_is_reported_for_both_paths() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 1920,
        height: 1080,
    };
    let tl = timeline_of("counter_1080p60.mkv", 600, res);
    let mut timings = Vec::new();
    for policy in [
        davimci_backend::DecodePolicy::Cpu,
        davimci_backend::DecodePolicy::Auto,
    ] {
        let mut b = MltBackend::new(tl.props).unwrap();
        let status = b.set_decode_policy(policy);
        b.set_timeline(&tl).unwrap();
        b.frame_at(Frame(0), PreviewScale::Full).unwrap();
        let start = Instant::now();
        for f in 1..=120u64 {
            b.frame_at(Frame(f), PreviewScale::Full).unwrap();
        }
        let per_frame = start.elapsed().as_secs_f64() * 1000.0 / 120.0;
        timings.push((policy, per_frame, status.is_hardware()));
    }
    for (policy, ms, hardware) in &timings {
        println!("decode {policy}: {ms:.2} ms/frame (hardware: {hardware})");
    }
    assert_eq!(timings.len(), 2);

    // Phase 3's number: the same pull without the RGBA conversion, and the
    // bytes a host would upload for each.
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();
    b.frame_at(Frame(0), PreviewScale::Full).unwrap();
    let start = Instant::now();
    let mut planar_bytes = 0;
    for f in 1..=120u64 {
        planar_bytes = b
            .planar_frame_at(Frame(f), PreviewScale::Full)
            .unwrap()
            .bytes();
    }
    let per_frame = start.elapsed().as_secs_f64() * 1000.0 / 120.0;
    println!(
        "planar pull: {per_frame:.2} ms/frame, {planar_bytes} bytes to upload against {} for RGBA8",
        (res.width as usize) * (res.height as usize) * 4
    );
}

#[test]
fn probe_reports_the_stream_graph_of_a_multitrack_mkv() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let mut b = MltBackend::new(TimelineProps::default()).unwrap();
    let info = b.probe(&fixtures().join("multitrack.mkv")).unwrap();
    assert!(info.has_video);
    assert_eq!(info.audio_streams, 3);
    assert_eq!(
        info.resolution,
        Some(Resolution {
            width: 640,
            height: 480
        })
    );
    assert!(info.frames > 0);
}

#[test]
fn preview_pulls_frames_and_advances_the_clock() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    b.preview_start(Frame(0), PreviewScale::Half).unwrap();
    assert!(b.is_previewing());

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut got = Vec::new();
    while got.len() < 3 && Instant::now() < deadline {
        match b.next_preview_frame().unwrap() {
            Some(f) => {
                assert!(f.is_well_formed());
                got.push(f.position);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    b.preview_stop().unwrap();
    assert!(!b.is_previewing());
    assert!(got.len() >= 3, "the preview produced no frames");
    assert!(
        got.windows(2).all(|w| w[1] >= w[0]),
        "presentation times went backwards: {got:?}"
    );
}

/// Regression: a backwards pass used to be MLT's own, which decodes every
/// frame with a seek and drops none of it, so the picture fell further behind
/// the sound the longer the shuttle ran. The clock has to keep real time and
/// the pictures have to arrive descending with it.
#[test]
fn a_backwards_shuttle_keeps_up_with_its_own_clock() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    b.preview_start(Frame(200), PreviewScale::Half).unwrap();
    b.set_rate(-1.0).unwrap();
    let (got, travelled) = drain_backwards(&mut b, Duration::from_secs(2));
    b.preview_stop().unwrap();

    assert!(
        travelled >= 60,
        "the clock covered {travelled} frames in two seconds of backwards play"
    );
    assert!(
        got.len() >= travelled as usize / 2,
        "{} pictures for {travelled} frames of sound: the picture is not keeping up",
        got.len()
    );
    assert!(
        got.windows(2).all(|w| w[1] <= w[0]),
        "a backwards pass handed over ascending frames: {got:?}"
    );
}

/// Regression: a fast backwards shuttle decoded runs it could never catch up
/// with, so every one of them was stale before it was ready and the preview
/// froze outright. Skipping frames is fine; showing none is not.
#[test]
fn a_fast_backwards_shuttle_keeps_showing_pictures() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = timeline_of("scene_cut.mkv", 240, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    for rate in [-2.0, -4.0, -8.0] {
        b.preview_start(Frame(239), PreviewScale::Half).unwrap();
        b.set_rate(rate).unwrap();
        let (got, travelled) = drain_backwards(&mut b, Duration::from_secs(1));
        b.preview_stop().unwrap();
        assert!(
            travelled > 0,
            "the clock did not run backwards at {rate:+}x"
        );
        assert!(
            got.len() >= 4,
            "{rate:+}x handed over {} pictures in a second: the preview is frozen",
            got.len()
        );
        assert!(
            got.windows(2).all(|w| w[1] <= w[0]),
            "{rate:+}x handed over ascending frames: {got:?}"
        );
    }
}

/// Pull frames for `window`, returning them and how far the clock travelled.
fn drain_backwards(b: &mut MltBackend, window: Duration) -> (Vec<Frame>, u64) {
    let started = Instant::now();
    let mut got: Vec<Frame> = Vec::new();
    let mut clock: Vec<Frame> = Vec::new();
    while started.elapsed() < window {
        while let Some(f) = b.next_preview_frame().unwrap() {
            assert!(f.is_well_formed());
            got.push(f.position);
        }
        if let Some(at) = b.audio_clock_position() {
            clock.push(at);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let travelled = clock
        .iter()
        .max()
        .zip(clock.iter().min())
        .map_or(0, |(hi, lo)| hi.get().saturating_sub(lo.get()));
    (got, travelled)
}

#[test]
fn a_render_produces_a_file_with_the_requested_streams() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 320,
        height: 240,
    };
    let tl = timeline_of("scene_cut.mkv", 120, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let out = std::env::temp_dir().join("davimci-render-smoke.mkv");
    let _ = std::fs::remove_file(&out);
    let settings = RenderSettings {
        resolution: res,
        fps: Fps::FPS_60,
        video_codec: "libx264".into(),
        audio_codec: "aac".into(),
        container: "mkv".into(),
        separate_audio_tracks: false,
        burn_subtitles: false,
        extra: vec![("preset".into(), "ultrafast".into())],
        hardware: davimci_backend::HardwareEncode::Off,
    };
    let mut job = RenderJob::new(&out, settings);
    job.range = Some((Frame::ZERO, Frame(60)));
    b.render(job).unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let p = b.progress();
        if p.state.is_terminal() {
            assert_eq!(p.state, RenderState::Done, "render did not complete");
            break;
        }
        assert!(Instant::now() < deadline, "render timed out");
        std::thread::sleep(Duration::from_millis(50));
    }

    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(&out)
        .output()
        .unwrap();
    let streams = String::from_utf8_lossy(&probe.stdout);
    assert!(streams.contains("h264"), "expected h264, got: {streams}");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn a_cancelled_render_stops_reporting_progress() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 320,
        height: 240,
    };
    let tl = timeline_of("counter_1080p60.mkv", 600, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    let out = std::env::temp_dir().join("davimci-render-cancel.mkv");
    let _ = std::fs::remove_file(&out);
    b.render(RenderJob::new(&out, RenderSettings::default()))
        .unwrap();
    b.cancel_render().unwrap();
    assert_eq!(b.progress().state, RenderState::Cancelled);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn an_unknown_clip_id_never_reaches_the_graph() {
    let _mlt = davimci_mlt::test_support::media_lock();
    // Guard against a projection that silently drops clips: every clip in the
    // timeline must appear in the XML the graph was built from.
    let res = Resolution {
        width: 320,
        height: 240,
    };
    let tl = timeline_of("scene_cut.mkv", 120, res);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();
    let xml = b.to_xml().unwrap();
    for track in tl.tracks() {
        for clip in track.clips() {
            assert!(
                xml.contains(&ClipId::to_string(&clip.id)),
                "clip {} is missing from the projection",
                clip.id
            );
        }
    }
}

/// The encode probe must tell a card that encodes from one that only
/// decodes: a render node is not an encode entrypoint, and trusting one is
/// how an export ends up as a container with no header.
///
/// The answer is whatever this machine can do; what is asserted is that the
/// probe reaches one, sticks to it, and that a device that does not exist is
/// never usable.
#[test]
fn the_hardware_encode_probe_is_decided_by_encoding_and_is_stable() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let mut b = MltBackend::new(TimelineProps::default()).unwrap();
    let first = b.hardware_encoder_probe("h264_vaapi", "/dev/dri/renderD128");
    let second = b.hardware_encoder_probe("h264_vaapi", "/dev/dri/renderD128");
    assert_eq!(first, second, "the probe must cache its answer");
    println!("h264_vaapi usable on this machine: {first}");

    assert!(
        !b.hardware_encoder_probe("h264_vaapi", "/dev/dri/renderD_nonexistent"),
        "a device that does not exist cannot encode"
    );
}

/// Two clips cut from `scene_cut.mkv` - a red one and a blue one - joined by
/// a dissolve, with handles on both sides for the overlap to borrow.
fn dissolve_timeline(res: Resolution, overlap: u64) -> Timeline {
    let mut tl = Timeline::new(TimelineProps {
        fps: Fps::FPS_60,
        resolution: res,
        sample_rate: 48_000,
    });
    let v1 = tl.track_by_name("V1").map(|t| t.id).unwrap();
    let media = || {
        MediaRef::new(
            fixtures()
                .join("scene_cut.mkv")
                .to_string_lossy()
                .to_string(),
            Fps::FPS_60,
            Frame(240),
        )
    };
    // Red is source 0-119, blue is 120-239, so each clip keeps 60 frames of
    // handle on the side the transition eats into.
    let red = Clip::from_media(
        ClipId(1),
        "red",
        media(),
        Frame::ZERO,
        Frame::ZERO,
        Frame(60),
    );
    let blue = Clip::from_media(ClipId(2), "blue", media(), Frame(60), Frame(180), Frame(60));
    tl.restore(v1, Frame::ZERO, &[red, blue], Frame(120), false)
        .unwrap();
    tl.set_transition(
        v1,
        ClipId(2),
        Some(davimci_core::Transition::new("dissolve", Frame(overlap))),
    )
    .unwrap();
    tl
}

/// The preview bug report: a dissolve that exists in the model has to reach
/// the stills the preview pulls, and reach them as an actual ramp.
///
/// Two ways of getting this wrong both look like "no transition": tracks of
/// the nested tractor with no in/out play source frame 0, and a transition
/// with no in/out takes its progress from the b-track producer's source
/// positions. Both are caught here, because both leave the ramp wrong rather
/// than absent.
#[test]
fn a_dissolve_blends_the_stills_the_preview_pulls() {
    let _mlt = davimci_mlt::test_support::media_lock();
    let res = Resolution {
        width: 640,
        height: 480,
    };
    let tl = dissolve_timeline(res, 20);
    let mut b = MltBackend::new(tl.props).unwrap();
    b.set_timeline(&tl).unwrap();

    // Red until the overlap, blue after it: the cut is at 60 and the 20-frame
    // overlap is centred on it, so 50-69 is the ramp.
    assert_eq!(
        dominant(&b.frame_at(Frame(40), PreviewScale::Full).unwrap()),
        0
    );
    assert_eq!(
        dominant(&b.frame_at(Frame(80), PreviewScale::Full).unwrap()),
        2
    );

    let ramp: Vec<[u8; 4]> = (50..70)
        .map(|f| {
            b.frame_at(Frame(f), PreviewScale::Full)
                .unwrap()
                .signature()
        })
        .collect();
    for (i, w) in ramp.windows(2).enumerate() {
        assert!(
            w[1][0] <= w[0][0] && w[1][2] >= w[0][2],
            "the dissolve is not monotonic at overlap frame {i}: {:?} then {:?}",
            w[0],
            w[1]
        );
    }
    // Halfway through, both sources are visibly present - the assertion the
    // bug failed: a transition that never composites keeps one of them at 0.
    let mid = ramp[ramp.len() / 2];
    assert!(
        mid[0] > 40 && mid[2] > 40,
        "the middle of the dissolve is not a blend: {mid:?}"
    );
    assert!(
        ramp[0][0] > 200 && ramp[ramp.len() - 1][2] > 200,
        "the ramp does not run from the outgoing clip to the incoming one"
    );
}
