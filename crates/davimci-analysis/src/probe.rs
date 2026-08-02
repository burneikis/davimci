//! Container probing (spec §7, plan.md Phase 5).
//!
//! Probing answers one question: what streams are in this file, and what
//! shape is each one? Every audio and subtitle stream in an MKV must be
//! visible here, because §7 requires each to become its own track.
//!
//! The parser is separated from the process launch on purpose: JSON in,
//! [`MediaInfo`] out is a pure function, so the whole stream-mapping matrix
//! is testable with no media and no `ffprobe` on the box. Only
//! [`FfprobeProber`] touches the outside world.

use std::path::Path;
use std::process::Command;

use davimci_core::{Fps, Resolution};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AnalysisError;

/// What a stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
}

/// One stream inside a container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Index within the container, as ffmpeg numbers it.
    pub index: u32,
    pub kind: StreamKind,
    pub codec: String,
    /// `title` metadata, e.g. "dialogue" - used to label the imported track.
    pub title: Option<String>,
    pub language: Option<String>,
    /// Video only: the stream's native rate, exact.
    pub fps: Option<Fps>,
    /// Video only.
    pub resolution: Option<Resolution>,
    /// Audio only.
    pub sample_rate: Option<u32>,
    /// Audio only.
    pub channels: Option<u32>,
    /// Length in *source* frames or samples, when the container states it.
    pub frames: Option<u64>,
    /// Bit depth, when stated. Part of the proxy threshold rule (spec §10.3).
    pub bit_depth: Option<u32>,
}

impl StreamInfo {
    #[must_use]
    pub fn label(&self) -> String {
        match (&self.title, &self.language) {
            (Some(t), _) => t.clone(),
            (None, Some(l)) => l.clone(),
            (None, None) => format!("{:?} {}", self.kind, self.index).to_lowercase(),
        }
    }
}

/// Everything a probe learned about one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub path: String,
    pub duration_seconds: f64,
    pub streams: Vec<StreamInfo>,
}

impl MediaInfo {
    #[must_use]
    pub fn streams_of(&self, kind: StreamKind) -> Vec<&StreamInfo> {
        self.streams.iter().filter(|s| s.kind == kind).collect()
    }

    /// The first video stream, which sets the file's framerate.
    #[must_use]
    pub fn video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.kind == StreamKind::Video)
    }

    /// The rate to conform from. Audio-only files have none, and are
    /// conformed by duration alone at the timeline rate.
    #[must_use]
    pub fn source_fps(&self) -> Option<Fps> {
        self.video().and_then(|v| v.fps)
    }

    /// Source length in its own frames at `fps`, from the container's frame
    /// count where it has one and from the duration otherwise.
    #[must_use]
    pub fn source_frames(&self, fps: Fps) -> u64 {
        if let Some(n) = self.video().and_then(|v| v.frames)
            && n > 0
        {
            return n;
        }
        (self.duration_seconds * fps.as_f64()).round().max(0.0) as u64
    }
}

/// Anything that can answer "what is in this file?".
///
/// A trait so the import pipeline can be tested against fixed answers with
/// no media present, per plan.md standing rule 1.
pub trait Prober: std::fmt::Debug {
    fn probe(&self, path: &Path) -> Result<MediaInfo, AnalysisError>;
}

/// The real prober: `ffprobe -show_streams -show_format -of json`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FfprobeProber;

impl Prober for FfprobeProber {
    fn probe(&self, path: &Path) -> Result<MediaInfo, AnalysisError> {
        let name = path.display().to_string();
        if !path.exists() {
            return Err(AnalysisError::MediaOffline { path: name });
        }
        let out = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    AnalysisError::ToolMissing {
                        tool: "ffprobe",
                        what: "media import",
                    }
                } else {
                    AnalysisError::io(&name, &e)
                }
            })?;
        if !out.status.success() {
            return Err(AnalysisError::ProbeFailed {
                path: name,
                reason: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        parse_ffprobe(&name, &String::from_utf8_lossy(&out.stdout))
    }
}

/// Parse `ffprobe -of json` output. Pure; the unit under test.
pub fn parse_ffprobe(path: &str, json: &str) -> Result<MediaInfo, AnalysisError> {
    let bad = |reason: String| AnalysisError::ProbeFailed {
        path: path.to_string(),
        reason,
    };
    let root: Value = serde_json::from_str(json).map_err(|e| bad(e.to_string()))?;
    let duration_seconds = root
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(Value::as_str)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    let mut streams = Vec::new();
    for s in root
        .get("streams")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let kind = match s.get("codec_type").and_then(Value::as_str) {
            Some("video") => StreamKind::Video,
            Some("audio") => StreamKind::Audio,
            Some("subtitle") => StreamKind::Subtitle,
            // Attachments and data streams are not editable content.
            _ => continue,
        };
        let tag = |k: &str| {
            s.get("tags")
                .and_then(|t| t.get(k))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let num = |k: &str| {
            s.get(k).and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            })
        };
        streams.push(StreamInfo {
            index: num("index").unwrap_or(0) as u32,
            kind,
            codec: s
                .get("codec_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            title: tag("title"),
            language: tag("language"),
            fps: if kind == StreamKind::Video {
                s.get("r_frame_rate").and_then(Value::as_str).and_then(rate)
            } else {
                None
            },
            resolution: match (num("width"), num("height")) {
                (Some(w), Some(h)) if w > 0 && h > 0 => Some(Resolution {
                    width: w as u32,
                    height: h as u32,
                }),
                _ => None,
            },
            sample_rate: num("sample_rate").map(|v| v as u32),
            channels: num("channels").map(|v| v as u32),
            frames: num("nb_frames").filter(|n| *n > 0),
            bit_depth: num("bits_per_raw_sample").map(|v| v as u32),
        });
    }

    if streams.is_empty() {
        return Err(AnalysisError::NoImportableStreams {
            path: path.to_string(),
        });
    }
    Ok(MediaInfo {
        path: path.to_string(),
        duration_seconds,
        streams,
    })
}

/// `"24000/1001"` -> exact [`Fps`]. Never a float.
fn rate(text: &str) -> Option<Fps> {
    let (n, d) = text.split_once('/')?;
    Fps::new(n.parse().ok()?, d.parse().ok()?).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;

    /// The shape `ffprobe` emits for `multitrack.mkv` from
    /// `scripts/gen-fixtures.sh`: 1 video, 3 named audio, 2 subtitle.
    pub(crate) const MULTITRACK_JSON: &str = r#"{
      "streams": [
        {"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":480,
         "r_frame_rate":"30/1","nb_frames":"150"},
        {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000",
         "channels":1,"tags":{"title":"dialogue","language":"eng"}},
        {"index":2,"codec_type":"audio","codec_name":"aac","sample_rate":"48000",
         "channels":1,"tags":{"title":"music"}},
        {"index":3,"codec_type":"audio","codec_name":"aac","sample_rate":"48000",
         "channels":1,"tags":{"title":"effects"}},
        {"index":4,"codec_type":"subtitle","codec_name":"subrip","tags":{"title":"s1"}},
        {"index":5,"codec_type":"subtitle","codec_name":"subrip","tags":{"title":"s2"}},
        {"index":6,"codec_type":"attachment","codec_name":"ttf"}
      ],
      "format": {"duration": "5.000000"}
    }"#;

    pub(crate) fn multitrack() -> MediaInfo {
        parse_ffprobe("/fixtures/multitrack.mkv", MULTITRACK_JSON).unwrap()
    }

    #[test]
    fn every_audio_and_subtitle_stream_is_exposed() {
        let info = multitrack();
        assert_eq!(info.streams_of(StreamKind::Video).len(), 1);
        assert_eq!(info.streams_of(StreamKind::Audio).len(), 3);
        assert_eq!(info.streams_of(StreamKind::Subtitle).len(), 2);
        // The attachment is not editable content and must not become a track.
        assert_eq!(info.streams.len(), 6);
        let titles: Vec<String> = info
            .streams_of(StreamKind::Audio)
            .iter()
            .map(|s| s.label())
            .collect();
        assert_eq!(titles, vec!["dialogue", "music", "effects"]);
    }

    #[test]
    fn stream_indices_are_preserved_for_the_track_mapping() {
        let info = multitrack();
        let idx: Vec<u32> = info
            .streams_of(StreamKind::Audio)
            .iter()
            .map(|s| s.index)
            .collect();
        assert_eq!(idx, vec![1, 2, 3]);
    }

    #[test]
    fn ntsc_rates_parse_as_exact_rationals() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
            "width":1920,"height":1080,"r_frame_rate":"24000/1001"}],
            "format":{"duration":"10.0"}}"#;
        let info = parse_ffprobe("/x.mkv", json).unwrap();
        assert_eq!(info.source_fps(), Some(Fps::FPS_23_976));
        assert_eq!(info.video().unwrap().resolution, Some(Resolution::HD_1080));
    }

    #[test]
    fn frame_count_falls_back_to_the_duration() {
        let json = r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
            "width":640,"height":480,"r_frame_rate":"30/1"}],
            "format":{"duration":"5.0"}}"#;
        let info = parse_ffprobe("/x.mkv", json).unwrap();
        assert_eq!(info.source_frames(Fps::FPS_30), 150);
    }

    #[test]
    fn a_stated_frame_count_wins_over_the_duration() {
        assert_eq!(multitrack().source_frames(Fps::FPS_30), 150);
    }

    #[test]
    fn a_file_with_nothing_importable_is_a_user_error() {
        let json = r#"{"streams":[{"index":0,"codec_type":"data"}],"format":{}}"#;
        assert!(matches!(
            parse_ffprobe("/x.bin", json),
            Err(AnalysisError::NoImportableStreams { .. })
        ));
    }

    #[test]
    fn junk_json_is_reported_not_panicked_on() {
        for junk in ["", "{", "null", "[1,2,3]", "{\"streams\":7}"] {
            let got = parse_ffprobe("/x.mkv", junk);
            assert!(got.is_err(), "{junk:?} should not have parsed");
        }
    }
}
