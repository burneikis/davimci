//! Proxy media.
//!
//! A proxy is generated when the source is above 1080p or uses a
//! long-GOP/expensive-to-seek codec; below that, the original decodes
//! directly. Proxies match the source framerate and frame count exactly, so
//! frame numbers are identical in both - a cut made on a proxy is the same
//! cut on the original.
//!
//! The hard invariant is at the other end: export always relinks to the
//! original. [`export_guard`] is the built-in `BeforeExport` check, and it
//! fails the render rather than quietly shipping 540p.

use std::path::{Path, PathBuf};

use davimci_core::{Fps, Frame, Timeline};
use serde::{Deserialize, Serialize};

use crate::conform::Conformed;
use crate::error::AnalysisError;
use crate::probe::{MediaInfo, StreamInfo};

/// Where a proxy encode decodes and scales its frames.
///
/// Decoding the source is most of the work of making a proxy, and the machine
/// that can encode on its GPU can usually decode there too. Which device is
/// available is a fact about the machine rather than a workflow opinion, so
/// [`Accel::Auto`] asks ffmpeg once and keeps the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accel {
    /// Whatever this machine turns out to have, software if nothing.
    Auto,
    /// Everything in software. The only mode that is always available.
    None,
    /// NVDEC decode and `scale_cuda`, frames never leaving the card.
    Cuda,
    /// VA-API decode and `scale_vaapi`, for Intel and AMD.
    Vaapi,
}

impl Accel {
    /// What to call this in a config or an error.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Cuda => "cuda",
            Self::Vaapi => "vaapi",
        }
    }

    /// Parse what a config wrote. Unknown names are refused rather than
    /// silently meaning software: a typo that halves the machine's speed
    /// should be visible.
    pub fn parse(name: &str) -> Result<Self, AnalysisError> {
        match name {
            "auto" => Ok(Self::Auto),
            "none" | "off" | "software" => Ok(Self::None),
            "cuda" | "nvdec" => Ok(Self::Cuda),
            "vaapi" => Ok(Self::Vaapi),
            other => Err(AnalysisError::UnknownAccel {
                name: other.to_string(),
            }),
        }
    }

    /// The ffmpeg options that put decoding on this device, before `-i`.
    fn input_args(self) -> Vec<String> {
        match self {
            Self::Auto | Self::None => Vec::new(),
            Self::Cuda => vec![
                "-hwaccel".into(),
                "cuda".into(),
                "-hwaccel_output_format".into(),
                "cuda".into(),
            ],
            Self::Vaapi => vec![
                "-hwaccel".into(),
                "vaapi".into(),
                "-hwaccel_output_format".into(),
                "vaapi".into(),
            ],
        }
    }

    /// The scaler that matches where the frames are. A hardware decode hands
    /// the filter graph frames on the device, which the software `scale`
    /// cannot read at all - the pair is not a preference, it is a
    /// requirement.
    fn scale_filter(self, width: u32, height: u32) -> String {
        match self {
            Self::Auto | Self::None => format!("scale={width}:{height}"),
            Self::Cuda => format!("scale_cuda={width}:{height}"),
            Self::Vaapi => format!("scale_vaapi={width}:{height}:format=nv12"),
        }
    }
}

fn default_accel() -> Accel {
    Accel::Auto
}

/// Proxy settings, mirroring `davimci.media.configure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyPolicy {
    pub auto: bool,
    pub height: u32,
    pub codec: String,
    /// Where the encode decodes and scales.
    #[serde(default = "default_accel")]
    pub accel: Accel,
    /// Sources taller than this get a proxy.
    pub max_native_height: u32,
    /// Codecs that are expensive to seek regardless of resolution.
    pub expensive_codecs: Vec<String>,
    /// Bit depths above 8 are expensive to decode in software.
    pub max_native_bit_depth: u32,
}

impl Default for ProxyPolicy {
    fn default() -> Self {
        Self {
            auto: true,
            height: 540,
            // ffmpeg spells ProRes Proxy as the `prores_ks` encoder at
            // profile 0; there is no `prores_proxy` encoder to ask for.
            codec: "prores_ks".into(),
            accel: default_accel(),
            max_native_height: 1080,
            expensive_codecs: vec!["hevc".into(), "h265".into(), "vp9".into(), "av1".into()],
            max_native_bit_depth: 8,
        }
    }
}

impl ProxyPolicy {
    /// `:set proxy off`.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            auto: false,
            ..Self::default()
        }
    }

    /// The threshold rule.
    #[must_use]
    pub fn needs_proxy(&self, stream: &StreamInfo) -> bool {
        if !self.auto {
            return false;
        }
        let tall = stream
            .resolution
            .is_some_and(|r| r.height > self.max_native_height);
        let expensive = self
            .expensive_codecs
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&stream.codec));
        let deep = stream
            .bit_depth
            .is_some_and(|b| b > self.max_native_bit_depth);
        tall || expensive || deep
    }
}

/// What to generate, and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    pub source: String,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Matches the source exactly, so frame numbers are identical.
    pub fps: Fps,
    pub frames: u64,
    pub codec: String,
    /// What the policy asked for. The encode may still fall back.
    pub accel: Accel,
}

/// Plan the proxy for a probed file, or `None` if it does not need one.
#[must_use]
pub fn plan_proxy(
    info: &MediaInfo,
    conformed: &Conformed,
    policy: &ProxyPolicy,
    cache_root: &Path,
    content_hash: &str,
) -> Option<ProxySpec> {
    let video = info.video()?;
    if !policy.needs_proxy(video) {
        return None;
    }
    let source = video.resolution?;
    // Preserve aspect, and keep both dimensions even for the encoder.
    let height = policy.height & !1;
    let scaled = u64::from(source.width) * u64::from(height) / u64::from(source.height.max(1));
    let width = u32::try_from(scaled).unwrap_or(u32::MAX) & !1;
    let fps = video.fps.unwrap_or(conformed.source_fps);
    Some(ProxySpec {
        source: info.path.clone(),
        path: cache_root.join(format!("{content_hash}.proxy.mov")),
        width: width.max(2),
        height: height.max(2),
        fps,
        frames: info.source_frames(fps),
        codec: policy.codec.clone(),
        accel: policy.accel,
    })
}

impl ProxySpec {
    /// Where the encode writes before it is finished.
    ///
    /// A `.mov` has no `moov` atom until ffmpeg closes it, so an encode
    /// killed part way through leaves a file that exists and cannot be
    /// decoded. Writing beside the real path and renaming on success means a
    /// file at [`ProxySpec::path`] is a complete one, and every reader can
    /// go on trusting that it exists.
    ///
    /// It keeps the extension: ffmpeg picks the muxer from it.
    #[must_use]
    pub fn partial_path(&self) -> PathBuf {
        let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
        let ext = self.path.extension().unwrap_or_default().to_string_lossy();
        self.path.with_file_name(format!("{stem}.partial.{ext}"))
    }

    /// The ffmpeg invocation that produces this proxy. Kept as data so the
    /// argument list is testable without encoding anything.
    #[must_use]
    pub fn ffmpeg_args(&self) -> Vec<String> {
        self.ffmpeg_args_on(self.accel)
    }

    /// The invocation that decodes and scales on `accel`.
    ///
    /// Split from [`ProxySpec::ffmpeg_args`] so a failed hardware run can be
    /// retried in software with the same spec, and so both forms are
    /// testable without a GPU.
    #[must_use]
    pub fn ffmpeg_args_on(&self, accel: Accel) -> Vec<String> {
        let mut args = vec![
            "-v".into(),
            "error".into(),
            "-nostats".into(),
            "-progress".into(),
            "pipe:2".into(),
            "-y".into(),
        ];
        args.extend(accel.input_args());
        args.extend([
            "-i".into(),
            self.source.clone(),
            "-map".into(),
            "0:v:0".into(),
            "-vf".into(),
            accel.scale_filter(self.width, self.height),
            "-r".into(),
            format!("{}/{}", self.fps.num, self.fps.den),
            "-c:v".into(),
            self.codec.clone(),
        ]);
        if self.codec.starts_with("prores") {
            // Profile 0 is Proxy - the point of the exercise.
            args.push("-profile:v".into());
            args.push("0".into());
        }
        args.push("-an".into());
        args.push(self.partial_path().display().to_string());
        args
    }

    /// How long the encode has to run, in microseconds of source media, so
    /// its progress can be a percentage rather than a spinner.
    #[must_use]
    pub fn duration_us(&self) -> u64 {
        if self.fps.num == 0 {
            return 0;
        }
        self.frames
            .saturating_mul(u64::from(self.fps.den))
            .saturating_mul(1_000_000)
            / u64::from(self.fps.num)
    }

    /// Timeline frames the proxy covers, at the timeline rate. Must equal the
    /// source's conformed length or the proxy is not interchangeable.
    #[must_use]
    pub fn conformed_length(&self, timeline_fps: Fps) -> Frame {
        timeline_fps.conform_frame(Frame(self.frames), self.fps)
    }
}

/// Whether a proxy already in the cache can be decoded.
///
/// Proxies written before the encode became atomic, or by any other means,
/// can be truncated containers: they exist, they have a plausible size, and
/// ffmpeg reports `moov atom not found` when the preview tries to play them.
/// One probe at import is cheaper than a session of blank pictures.
#[must_use]
pub fn is_usable(path: &Path) -> bool {
    std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .is_ok_and(|out| out.status.success() && !out.stdout.is_empty())
}

/// Encode a proxy with ffmpeg. Cancellable, since a 4K transcode is the
/// longest job davimci runs.
pub fn generate(spec: &ProxySpec, phase: crate::jobs::Phase<'_>) -> Result<(), AnalysisError> {
    let wanted = match spec.accel {
        Accel::Auto => detect_accel(),
        chosen => chosen,
    };
    match encode(spec, wanted, phase) {
        // Hardware that is present can still refuse this particular file -
        // an unsupported codec, a busy device, a driver that went away. The
        // proxy is worth more than the speed, so it is encoded again in
        // software rather than lost.
        Err(AnalysisError::AnalysisFailed { .. }) if wanted != Accel::None => {
            encode(spec, Accel::None, phase)
        }
        other => other,
    }
}

fn encode(
    spec: &ProxySpec,
    accel: Accel,
    phase: crate::jobs::Phase<'_>,
) -> Result<(), AnalysisError> {
    let total_us = spec.duration_us();
    phase.check()?;
    phase.report(0, total_us.max(1));
    if let Some(parent) = spec.path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AnalysisError::CacheUnwritable {
            reason: e.to_string(),
        })?;
    }
    let partial = spec.partial_path();
    let mut command = std::process::Command::new("ffmpeg");
    command.args(spec.ffmpeg_args_on(accel));
    let out = crate::run::output_with_progress(&mut command, phase.ctx(), |us| {
        phase.report(us, total_us);
    })
    .map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        if e.kind() == std::io::ErrorKind::NotFound {
            AnalysisError::ToolMissing {
                tool: "ffmpeg",
                what: "proxy generation",
            }
        } else {
            AnalysisError::io(&spec.source, &e)
        }
    })?;
    // Killed part way through: the partial container is worthless, and the
    // caller is waiting on this thread to return.
    let Some(out) = out else {
        let _ = std::fs::remove_file(&partial);
        return Err(AnalysisError::Cancelled);
    };
    if !out.status.success() {
        // Half a container is worse than none: it exists, so every later
        // check takes it for a finished proxy.
        let _ = std::fs::remove_file(&partial);
        return Err(AnalysisError::AnalysisFailed {
            path: spec.source.clone(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    // The rename is what publishes the proxy, and it is atomic within the
    // cache directory: a reader sees the whole file or no file.
    std::fs::rename(&partial, &spec.path).map_err(|e| AnalysisError::CacheUnwritable {
        reason: e.to_string(),
    })?;
    phase.report(1, 1);
    Ok(())
}

/// The best decoder this machine actually has, asked once.
///
/// Listed support is not working support - a build can name `cuda` on a
/// machine with no card - so each candidate is tried on a frame of generated
/// video before it is believed. The trial costs a fraction of a second, once
/// per process, and only when a proxy is first encoded.
pub fn detect_accel() -> Accel {
    static FOUND: std::sync::OnceLock<Accel> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        [Accel::Cuda, Accel::Vaapi]
            .into_iter()
            .find(|accel| accel_works(*accel))
            .unwrap_or(Accel::None)
    })
}

/// Whether ffmpeg can open this device and scale on it.
///
/// A generated frame is uploaded and scaled rather than decoded: `-hwaccel`
/// applies to a decoded stream, so trying it on `lavfi` proves nothing and
/// fails even where the device is perfectly good. Whether the device's
/// *decoder* also takes a particular codec is not knowable without that
/// file, which is what the fallback in [`generate`] is for.
fn accel_works(accel: Accel) -> bool {
    let device = match accel {
        Accel::Cuda => "cuda=dev",
        Accel::Vaapi => "vaapi=dev",
        Accel::Auto | Accel::None => return true,
    };
    std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-init_hw_device",
            device,
            "-filter_hw_device",
            "dev",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=64x64:rate=1:duration=1",
            "-vf",
            &format!("format=nv12,hwupload,{}", accel.scale_filter(32, 32)),
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Which proxy stands in for which original, for the duration of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyMap {
    entries: Vec<(String, String)>,
}

impl ProxyMap {
    pub fn insert(&mut self, proxy: impl Into<String>, original: impl Into<String>) {
        self.entries.push((proxy.into(), original.into()));
    }

    /// The original a path resolves to for export, which for anything that is
    /// not a proxy is the path itself.
    #[must_use]
    pub fn original_of<'a>(&'a self, path: &'a str) -> &'a str {
        self.entries
            .iter()
            .find(|(proxy, _)| proxy == path)
            .map_or(path, |(_, original)| original.as_str())
    }

    /// The proxy standing in for a source, if one has finished encoding.
    ///
    /// The inverse of [`ProxyMap::original_of`], and what the preview
    /// resolves a clip's media through: the timeline itself holds the
    /// original, so a proxy is never saved, exported or undone.
    #[must_use]
    pub fn proxy_for<'a>(&'a self, source: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .find(|(_, original)| original == source)
            .map(|(proxy, _)| proxy.as_str())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn is_proxy(&self, path: &str) -> bool {
        self.entries.iter().any(|(proxy, _)| proxy == path)
    }
}

/// The built-in `BeforeExport` check.
///
/// Fails the render if any clip would resolve to a proxy. It reports the
/// first offender by name, because "some clip somewhere" is not an actionable
/// status line.
pub fn export_guard(tl: &Timeline, proxies: &ProxyMap) -> Result<(), AnalysisError> {
    for track in tl.tracks() {
        for clip in track.clips() {
            if let Some(m) = &clip.media
                && proxies.is_proxy(&m.path)
            {
                return Err(AnalysisError::ProxyInExport {
                    clip: clip.label.clone(),
                    path: m.path.clone(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::conform::{ConformOptions, conform};
    use crate::probe::{StreamKind, parse_ffprobe};
    use davimci_core::{Clip, MediaRef, Resolution, TimelineProps};

    fn stream(width: u32, height: u32, codec: &str, bit_depth: Option<u32>) -> StreamInfo {
        StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: codec.into(),
            title: None,
            language: None,
            fps: Some(Fps::FPS_30),
            resolution: Some(Resolution { width, height }),
            sample_rate: None,
            channels: None,
            frames: Some(90),
            bit_depth,
        }
    }

    /// The threshold rule across the resolution/codec matrix.
    #[test]
    fn the_threshold_rule_picks_correctly() {
        let p = ProxyPolicy::default();
        let cases = [
            // (width, height, codec, bit depth, wants a proxy)
            (1920, 1080, "h264", None, false),
            (1280, 720, "h264", None, false),
            (3840, 2160, "h264", None, true),
            (1920, 1080, "hevc", None, true),
            (1280, 720, "vp9", None, true),
            (1920, 1080, "h264", Some(10), true),
            (1920, 1080, "prores", None, false),
        ];
        for (w, h, codec, depth, want) in cases {
            assert_eq!(
                p.needs_proxy(&stream(w, h, codec, depth)),
                want,
                "{w}x{h} {codec} {depth:?}"
            );
        }
    }

    #[test]
    fn proxies_can_be_turned_off_entirely() {
        let p = ProxyPolicy::disabled();
        assert!(!p.needs_proxy(&stream(3840, 2160, "hevc", Some(10))));
    }

    fn info_4k() -> MediaInfo {
        parse_ffprobe(
            "/fixtures/counter_4k.mkv",
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
               "width":3840,"height":2160,"r_frame_rate":"30/1","nb_frames":"90"}],
             "format":{"duration":"3.0"}}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_proxy_matches_the_source_framerate_and_frame_count_exactly() {
        let info = info_4k();
        let c = conform(&info, TimelineProps::default(), ConformOptions::default());
        let spec = plan_proxy(
            &info,
            &c,
            &ProxyPolicy::default(),
            Path::new("/p/.davimci/cache"),
            "abc123",
        )
        .unwrap();
        assert_eq!(spec.fps, Fps::FPS_30);
        assert_eq!(spec.frames, 90);
        assert_eq!((spec.width, spec.height), (960, 540));
        assert_eq!(
            spec.conformed_length(TimelineProps::default().fps),
            c.length,
            "the proxy must cover exactly the same timeline range"
        );
        assert!(spec.ffmpeg_args().contains(&"scale=960:540".to_string()));
        assert!(spec.ffmpeg_args().contains(&"30/1".to_string()));
        assert!(spec.ffmpeg_args().contains(&"-profile:v".to_string()));
    }

    /// The decoder and the scaler are a pair: frames decoded on the card
    /// cannot be read by the software `scale`, so asking for one without the
    /// other produces a command that always fails.
    #[test]
    fn a_hardware_decode_brings_its_own_scaler() {
        let info = info_4k();
        let c = conform(&info, TimelineProps::default(), ConformOptions::default());
        let spec = plan_proxy(
            &info,
            &c,
            &ProxyPolicy::default(),
            Path::new("/p/.davimci/cache"),
            "abc123",
        )
        .unwrap();

        let cuda = spec.ffmpeg_args_on(Accel::Cuda);
        assert!(cuda.contains(&"scale_cuda=960:540".to_string()));
        assert!(cuda.contains(&"-hwaccel_output_format".to_string()));
        let at = |args: &[String], flag: &str| args.iter().position(|a| a == flag);
        assert!(
            at(&cuda, "-hwaccel") < at(&cuda, "-i"),
            "-hwaccel is an input option and must precede -i"
        );

        let vaapi = spec.ffmpeg_args_on(Accel::Vaapi);
        assert!(vaapi.contains(&"scale_vaapi=960:540:format=nv12".to_string()));

        let soft = spec.ffmpeg_args_on(Accel::None);
        assert!(soft.contains(&"scale=960:540".to_string()));
        assert!(
            !soft.iter().any(|a| a == "-hwaccel"),
            "software asked for a device"
        );
    }

    #[test]
    fn an_unknown_decoder_is_refused_rather_than_meaning_software() {
        assert_eq!(Accel::parse("cuda").unwrap(), Accel::Cuda);
        assert_eq!(Accel::parse("off").unwrap(), Accel::None);
        let err = Accel::parse("cude").unwrap_err();
        assert_eq!(
            davimci_core::Classify::class(&err),
            davimci_core::ErrorClass::User
        );
        assert!(err.to_string().contains("cuda"), "{err}");
    }

    /// Regression: the encode sat at 0% until it finished. It has to ask
    /// ffmpeg where it is, and know how long the source runs to divide by.
    #[test]
    fn an_encode_asks_ffmpeg_for_progress_against_a_known_duration() {
        let info = info_4k();
        let c = conform(&info, TimelineProps::default(), ConformOptions::default());
        let spec = plan_proxy(
            &info,
            &c,
            &ProxyPolicy::default(),
            Path::new("/p/.davimci/cache"),
            "abc123",
        )
        .unwrap();
        assert_eq!(spec.duration_us(), 3_000_000, "90 frames at 30 fps is 3 s");
        let args = spec.ffmpeg_args();
        assert!(args.contains(&"-progress".to_string()));
        assert!(args.contains(&"pipe:2".to_string()));
    }

    #[test]
    fn a_1080p_source_gets_no_proxy() {
        let info = parse_ffprobe(
            "/x.mkv",
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
               "width":1920,"height":1080,"r_frame_rate":"60/1"}],"format":{"duration":"1"}}"#,
        )
        .unwrap();
        let c = conform(&info, TimelineProps::default(), ConformOptions::default());
        assert!(plan_proxy(&info, &c, &ProxyPolicy::default(), Path::new("/tmp"), "abc").is_none());
    }

    #[test]
    fn odd_dimensions_are_rounded_to_even_for_the_encoder() {
        let info = parse_ffprobe(
            "/x.mkv",
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"hevc",
               "width":1439,"height":1079,"r_frame_rate":"30/1"}],"format":{"duration":"1"}}"#,
        )
        .unwrap();
        let c = conform(&info, TimelineProps::default(), ConformOptions::default());
        let spec = plan_proxy(&info, &c, &ProxyPolicy::default(), Path::new("/tmp"), "a").unwrap();
        assert_eq!(spec.width % 2, 0);
        assert_eq!(spec.height % 2, 0);
    }

    fn timeline_using(path: &str) -> Timeline {
        let mut tl = Timeline::new(TimelineProps::default());
        let v1 = tl.tracks()[0].id;
        let id = tl.new_clip_id();
        let media = MediaRef::new(path, Fps::FPS_60, Frame(600));
        let clip = Clip::from_media(id, "shot", media, Frame::ZERO, Frame::ZERO, Frame(120));
        tl.restore(v1, Frame::ZERO, &[clip], Frame(120), false)
            .unwrap();
        tl
    }

    #[test]
    fn export_is_refused_while_a_clip_resolves_to_a_proxy() {
        let mut proxies = ProxyMap::default();
        proxies.insert("/p/.davimci/cache/abc.proxy.mov", "/media/original.mkv");

        let ok = timeline_using("/media/original.mkv");
        assert!(export_guard(&ok, &proxies).is_ok());

        let bad = timeline_using("/p/.davimci/cache/abc.proxy.mov");
        let err = export_guard(&bad, &proxies).unwrap_err();
        match &err {
            AnalysisError::ProxyInExport { clip, .. } => assert_eq!(clip, "shot"),
            other => panic!("wrong error: {other:?}"),
        }
        // It must say what to do about it, not just that it failed.
        assert!(davimci_core::Classify::user_message(&err).contains("original"));
    }

    #[test]
    fn export_relinks_a_proxy_path_to_its_original() {
        let mut proxies = ProxyMap::default();
        proxies.insert("/cache/abc.proxy.mov", "/media/original.mkv");
        assert_eq!(
            proxies.original_of("/cache/abc.proxy.mov"),
            "/media/original.mkv"
        );
        assert_eq!(proxies.original_of("/media/other.mkv"), "/media/other.mkv");
    }

    /// The preview resolves the other way: the timeline names the original,
    /// and the graph asks what stands in for it.
    #[test]
    fn the_preview_looks_up_the_proxy_standing_in_for_a_source() {
        let mut proxies = ProxyMap::default();
        assert!(proxies.is_empty());
        proxies.insert("/cache/abc.proxy.mov", "/media/original.mkv");
        assert!(!proxies.is_empty());
        assert_eq!(
            proxies.proxy_for("/media/original.mkv"),
            Some("/cache/abc.proxy.mov")
        );
        assert_eq!(proxies.proxy_for("/media/other.mkv"), None);
    }
}
