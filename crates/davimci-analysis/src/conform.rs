//! The conform stage.
//!
//! Everything downstream of import sees a single-rate, single-resolution
//! timeline, so every source is conformed on the way in:
//!
//! - framerate: the source's frame count is mapped to timeline frames by
//!   [`Fps::conform_frame`], which is nearest-frame and computed per boundary,
//!   so a long clip cannot accumulate drift;
//! - resolution: a fit rectangle, letterbox/pillarbox or crop-to-fill;
//! - audio: resampled to the project sample rate.
//!
//! All of it is display-and-render transformation. The original file is never
//! touched and export relinks to it.

use davimci_core::{Fps, Frame, Resolution, TimelineProps};
use serde::{Deserialize, Serialize};

use crate::probe::{MediaInfo, StreamKind};

/// What to do when the source aspect and the timeline aspect disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FitPolicy {
    /// Fit inside the frame, with bars. Nothing is lost.
    #[default]
    Letterbox,
    /// Fill the frame, cropping the overflow.
    Crop,
}

/// Per-import conform settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConformOptions {
    pub fit: FitPolicy,
}

/// Where a conformed source lands inside the timeline frame.
///
/// Coordinates are signed because a crop places the image *outside* the
/// frame on two edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FitRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl FitRect {
    /// Whether the source needed bars (letterbox/pillarbox).
    #[must_use]
    pub fn has_bars(&self, target: Resolution) -> bool {
        self.width < target.width || self.height < target.height
    }
}

/// Scale `source` into `target` under `policy`, preserving aspect ratio.
#[must_use]
pub fn fit(source: Resolution, target: Resolution, policy: FitPolicy) -> FitRect {
    if source.width == 0 || source.height == 0 {
        return FitRect {
            x: 0,
            y: 0,
            width: target.width,
            height: target.height,
        };
    }
    let sx = f64::from(target.width) / f64::from(source.width);
    let sy = f64::from(target.height) / f64::from(source.height);
    let scale = match policy {
        FitPolicy::Letterbox => sx.min(sy),
        FitPolicy::Crop => sx.max(sy),
    };
    let width = (f64::from(source.width) * scale).round().max(1.0) as u32;
    let height = (f64::from(source.height) * scale).round().max(1.0) as u32;
    FitRect {
        x: (i64::from(target.width) - i64::from(width)) as i32 / 2,
        y: (i64::from(target.height) - i64::from(height)) as i32 / 2,
        width,
        height,
    }
}

/// How one source becomes a clip on the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Conformed {
    /// The source's own rate, kept for export relink and re-conform.
    pub source_fps: Fps,
    /// Source length expressed in whole timeline frames.
    pub length: Frame,
    /// Where the video lands in the timeline frame, if there is video.
    pub rect: Option<FitRect>,
    /// True when the audio has to be resampled to the project rate.
    pub resample_audio: bool,
}

/// Conform a probed file to `props`.
///
/// The framerate a file is conformed *from* is its video stream's rate;
/// audio-only files carry no rate of their own, so they are measured in
/// timeline frames directly and conform is the identity.
#[must_use]
pub fn conform(info: &MediaInfo, props: TimelineProps, opts: ConformOptions) -> Conformed {
    let source_fps = info.source_fps().unwrap_or(props.fps);
    let source_frames = info.source_frames(source_fps);
    let length = props.fps.conform_frame(Frame(source_frames), source_fps);
    let rect = info
        .video()
        .and_then(|v| v.resolution)
        .map(|r| fit(r, props.resolution, opts.fit));
    let resample_audio = info
        .streams_of(StreamKind::Audio)
        .iter()
        .any(|s| s.sample_rate.is_some_and(|r| r != props.sample_rate));
    Conformed {
        source_fps,
        length: Frame(length.get().max(1)),
        rect,
        resample_audio,
    }
}

/// Milliseconds to the nearest timeline frame.
///
/// Analysis works in milliseconds because a hop is a property of the audio,
/// not of the timeline; this is the single conversion point back. Nearest
/// rather than containing, so that a frame's own start time maps back to it:
/// the truncation in [`ms_at_frame`] would otherwise land a frame early.
#[must_use]
pub fn frame_at_ms(ms: u64, fps: Fps) -> Frame {
    let num = u128::from(ms) * u128::from(fps.num);
    let den = u128::from(fps.den) * 1000;
    Frame(((num + den / 2) / den) as u64)
}

/// The inverse of [`frame_at_ms`]: the start of `frame`, in milliseconds.
#[must_use]
pub fn ms_at_frame(frame: Frame, fps: Fps) -> u64 {
    let num = u128::from(frame.get()) * u128::from(fps.den) * 1000;
    (num / u128::from(fps.num)) as u64
}

/// Timeline properties defaulted from the first import.
#[must_use]
pub fn props_from(info: &MediaInfo, fallback: TimelineProps) -> TimelineProps {
    TimelineProps {
        fps: info.source_fps().unwrap_or(fallback.fps),
        resolution: info
            .video()
            .and_then(|v| v.resolution)
            .unwrap_or(fallback.resolution),
        sample_rate: info
            .streams_of(StreamKind::Audio)
            .first()
            .and_then(|s| s.sample_rate)
            .unwrap_or(fallback.sample_rate),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::probe::parse_ffprobe;

    fn info(width: u32, height: u32, rate: &str, seconds: f64) -> MediaInfo {
        let json = format!(
            r#"{{"streams":[{{"index":0,"codec_type":"video","codec_name":"h264",
               "width":{width},"height":{height},"r_frame_rate":"{rate}"}},
               {{"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"44100",
                 "channels":2}}],
             "format":{{"duration":"{seconds}"}}}}"#
        );
        parse_ffprobe("/x.mkv", &json).unwrap()
    }

    fn hd60() -> TimelineProps {
        TimelineProps::default()
    }

    #[test]
    fn same_aspect_scales_edge_to_edge() {
        let r = fit(
            Resolution {
                width: 1280,
                height: 720,
            },
            Resolution::HD_1080,
            FitPolicy::Letterbox,
        );
        assert_eq!(
            r,
            FitRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            }
        );
        assert!(!r.has_bars(Resolution::HD_1080));
    }

    #[test]
    fn a_wider_source_letterboxes_and_a_crop_fills() {
        let anamorphic = Resolution {
            width: 2560,
            height: 1080,
        };
        let bars = fit(anamorphic, Resolution::HD_1080, FitPolicy::Letterbox);
        assert_eq!(bars.width, 1920);
        assert_eq!(bars.height, 810);
        assert_eq!(bars.y, 135);
        assert!(bars.has_bars(Resolution::HD_1080));

        let filled = fit(anamorphic, Resolution::HD_1080, FitPolicy::Crop);
        assert_eq!(filled.height, 1080);
        assert_eq!(filled.width, 2560);
        // A crop hangs off both sides, symmetrically.
        assert_eq!(filled.x, -320);
        assert!(!filled.has_bars(Resolution::HD_1080));
    }

    #[test]
    fn a_taller_source_pillarboxes() {
        let vertical = Resolution {
            width: 1080,
            height: 1920,
        };
        let r = fit(vertical, Resolution::HD_1080, FitPolicy::Letterbox);
        assert_eq!((r.width, r.height), (608, 1080));
        assert_eq!(r.x, 656);
    }

    /// Conform matrix: every common rate and size into 1080p60.
    #[test]
    fn the_conform_matrix_lands_on_exact_whole_frames() {
        let cases = [
            // (w, h, rate, seconds, expected timeline frames at 60fps)
            (1920, 1080, "60/1", 10.0, 600),
            (1920, 1080, "30/1", 10.0, 600),
            (1920, 1080, "25/1", 10.0, 600),
            // 10s of 23.976 is 240 source frames, which is 10.01s of real
            // time and so 601 timeline frames. Conform follows the frames,
            // not the container's rounded duration.
            (1920, 1080, "24000/1001", 10.0, 601),
            (1280, 720, "60/1", 5.0, 300),
            (3840, 2160, "30/1", 3.0, 180),
        ];
        for (w, h, rate, secs, want) in cases {
            let c = conform(&info(w, h, rate, secs), hd60(), ConformOptions::default());
            assert_eq!(
                c.length,
                Frame(want),
                "{w}x{h} @ {rate} for {secs}s conformed wrong"
            );
        }
    }

    #[test]
    fn a_long_source_does_not_accumulate_drift() {
        // An hour of 23.976 into 60fps. The mapping is computed once from the
        // source frame count, so the error is bounded at half a frame after
        // an hour exactly as it is after one frame; stepping frame by frame
        // would drift by seconds.
        let source = info(1920, 1080, "24000/1001", 3600.0);
        let c = conform(&source, hd60(), ConformOptions::default());
        let frames = source.source_frames(Fps::FPS_23_976) as f64;
        let want = frames * (1001.0 / 24_000.0) * 60.0;
        assert!(
            (c.length.get() as f64 - want).abs() <= 0.5,
            "drifted to {} frames, wanted {want}",
            c.length
        );
    }

    #[test]
    fn audio_at_another_rate_is_flagged_for_resampling() {
        let c = conform(
            &info(1920, 1080, "60/1", 1.0),
            hd60(),
            ConformOptions::default(),
        );
        assert!(
            c.resample_audio,
            "44.1 kHz into a 48 kHz project must resample"
        );
    }

    #[test]
    fn the_first_import_sets_the_project_properties() {
        let p = props_from(&info(3840, 2160, "24000/1001", 1.0), hd60());
        assert_eq!(p.fps, Fps::FPS_23_976);
        assert_eq!(p.resolution.width, 3840);
        assert_eq!(p.sample_rate, 44_100);
    }

    #[test]
    fn milliseconds_and_frames_round_trip_at_ntsc_rates() {
        for fps in [Fps::FPS_60, Fps::FPS_25, Fps::FPS_23_976] {
            for frame in [0u64, 1, 999, 86_400] {
                let ms = ms_at_frame(Frame(frame), fps);
                assert_eq!(
                    frame_at_ms(ms, fps),
                    Frame(frame),
                    "frame {frame} at {fps} fps did not survive {ms} ms"
                );
            }
        }
    }

    #[test]
    fn a_clip_never_conforms_away_to_nothing() {
        // Two frames of 23.976 into a 1fps timeline rounds to zero frames;
        // a clip that exists must stay on the timeline.
        let props = TimelineProps {
            fps: Fps::new(1, 1).unwrap(),
            ..hd60()
        };
        let c = conform(
            &info(1920, 1080, "24000/1001", 0.08),
            props,
            ConformOptions::default(),
        );
        assert!(c.length >= Frame(1));
    }
}
