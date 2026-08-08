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

/// Proxy settings, mirroring `davimci.media.configure`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyPolicy {
    pub auto: bool,
    pub height: u32,
    pub codec: String,
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
    })
}

impl ProxySpec {
    /// The ffmpeg invocation that produces this proxy. Kept as data so the
    /// argument list is testable without encoding anything.
    #[must_use]
    pub fn ffmpeg_args(&self) -> Vec<String> {
        let mut args = vec![
            "-v".into(),
            "error".into(),
            "-y".into(),
            "-i".into(),
            self.source.clone(),
            "-map".into(),
            "0:v:0".into(),
            "-vf".into(),
            format!("scale={}:{}", self.width, self.height),
            "-r".into(),
            format!("{}/{}", self.fps.num, self.fps.den),
            "-c:v".into(),
            self.codec.clone(),
        ];
        if self.codec.starts_with("prores") {
            // Profile 0 is Proxy - the point of the exercise.
            args.push("-profile:v".into());
            args.push("0".into());
        }
        args.push("-an".into());
        args.push(self.path.display().to_string());
        args
    }

    /// Timeline frames the proxy covers, at the timeline rate. Must equal the
    /// source's conformed length or the proxy is not interchangeable.
    #[must_use]
    pub fn conformed_length(&self, timeline_fps: Fps) -> Frame {
        timeline_fps.conform_frame(Frame(self.frames), self.fps)
    }
}

/// Encode a proxy with ffmpeg. Cancellable, since a 4K transcode is the
/// longest job davimci runs.
pub fn generate(
    spec: &ProxySpec,
    ctx: Option<&crate::jobs::JobContext>,
) -> Result<(), AnalysisError> {
    if let Some(ctx) = ctx {
        ctx.check()?;
        ctx.progress(0, 1);
    }
    if let Some(parent) = spec.path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AnalysisError::CacheUnwritable {
            reason: e.to_string(),
        })?;
    }
    let out = std::process::Command::new("ffmpeg")
        .args(spec.ffmpeg_args())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AnalysisError::ToolMissing {
                    tool: "ffmpeg",
                    what: "proxy generation",
                }
            } else {
                AnalysisError::io(&spec.source, &e)
            }
        })?;
    if !out.status.success() {
        return Err(AnalysisError::AnalysisFailed {
            path: spec.source.clone(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    if let Some(ctx) = ctx {
        ctx.progress(1, 1);
    }
    Ok(())
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
