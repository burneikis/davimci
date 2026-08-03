//! Export presets defined from Lua (spec §9.5).
//!
//! Validation happens where the preset is *defined*, not where it is used:
//! a misspelled container is a user error (Phase 0), and the user should
//! hear about it when the config loads rather than after a long render.
//! Phase 8b owns the registry that actually runs these; this is the part
//! that can exist without an encoder.

use davimci_backend::RenderSettings;
use davimci_core::{Fps, Resolution};

use crate::error::LuaError;

/// Which audio tracks an export includes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackSelection {
    All,
    None,
    Named(Vec<String>),
}

/// What an export does with subtitle tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtitleSelection {
    /// Rendered into the video.
    Burned,
    /// Written next to the output as a separate file.
    Sidecar,
    /// Muxed as subtitle streams.
    Embedded,
    None,
    Named(Vec<String>),
}

/// A validated export preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPreset {
    pub name: String,
    pub container: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub audio_tracks: TrackSelection,
    pub subtitle_tracks: SubtitleSelection,
}

/// Containers davimci will mux, and the codecs each accepts. A pairing outside
/// this table is rejected with a sentence naming both halves.
const CONTAINERS: &[(&str, &[&str], &[&str])] = &[
    (
        "mkv",
        &["h264", "h265", "vp9", "prores"],
        &["aac", "opus", "flac", "pcm"],
    ),
    ("mp4", &["h264", "h265"], &["aac"]),
    ("mov", &["h264", "prores"], &["aac", "pcm"]),
    ("webm", &["vp9"], &["opus"]),
];

/// Spec §10.3's rule: a preset names a codec, the backend gets an ffmpeg
/// *encoder*. Keeping the mapping here is what stops a marketing name from
/// reaching the command line.
fn video_encoder(codec: &str) -> Option<&'static str> {
    Some(match codec {
        "h264" => "libx264",
        "h265" => "libx265",
        "vp9" => "libvpx-vp9",
        "prores" => "prores_ks",
        _ => return None,
    })
}

fn audio_encoder(codec: &str) -> Option<&'static str> {
    Some(match codec {
        "aac" => "aac",
        "opus" => "libopus",
        "flac" => "flac",
        "pcm" => "pcm_s16le",
        _ => return None,
    })
}

/// Parse `1920x1080` into a [`Resolution`].
pub(crate) fn parse_resolution(s: &str) -> Option<Resolution> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some(Resolution {
        width: w.trim().parse().ok()?,
        height: h.trim().parse().ok()?,
    })
}

impl ExportPreset {
    /// Reject anything that could not encode, with a complete sentence.
    pub fn validate(&self) -> Result<(), LuaError> {
        let Some((_, video, audio)) = CONTAINERS
            .iter()
            .find(|(c, _, _)| *c == self.container)
            .copied()
        else {
            let known: Vec<&str> = CONTAINERS.iter().map(|(c, _, _)| *c).collect();
            return Err(LuaError::Config(format!(
                "export preset '{}' asks for container '{}', which davimci cannot write (known: {})",
                self.name,
                self.container,
                known.join(", ")
            )));
        };
        if !video.contains(&self.video_codec.as_str()) {
            return Err(LuaError::Config(format!(
                "export preset '{}' pairs video codec '{}' with container '{}', which cannot hold it (accepts: {})",
                self.name,
                self.video_codec,
                self.container,
                video.join(", ")
            )));
        }
        if !audio.contains(&self.audio_codec.as_str()) {
            return Err(LuaError::Config(format!(
                "export preset '{}' pairs audio codec '{}' with container '{}', which cannot hold it (accepts: {})",
                self.name,
                self.audio_codec,
                self.container,
                audio.join(", ")
            )));
        }
        if let SubtitleSelection::Embedded = self.subtitle_tracks
            && self.container == "mp4"
        {
            return Err(LuaError::Config(format!(
                "export preset '{}' embeds subtitles in an mp4; use 'burned' or 'sidecar' instead",
                self.name
            )));
        }
        Ok(())
    }

    /// The encoder settings the backend needs. Anything the preset leaves
    /// open falls back to the timeline's own geometry, supplied by the
    /// caller, since a preset must not silently reframe a project.
    #[must_use]
    pub fn render_settings(&self, timeline_res: Resolution, timeline_fps: Fps) -> RenderSettings {
        RenderSettings {
            resolution: self.resolution.unwrap_or(timeline_res),
            fps: self.fps.unwrap_or(timeline_fps),
            video_codec: video_encoder(&self.video_codec)
                .unwrap_or("libx264")
                .to_string(),
            audio_codec: audio_encoder(&self.audio_codec)
                .unwrap_or("aac")
                .to_string(),
            container: self.container.clone(),
            // Matroska is the container that keeps every audio track as its
            // own stream (spec §7); anywhere else they are mixed.
            separate_audio_tracks: matches!(self.container.as_str(), "mkv" | "matroska")
                && self.audio_tracks != TrackSelection::None,
            extra: Vec::new(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_core::Classify;

    fn preset(container: &str, v: &str, a: &str) -> ExportPreset {
        ExportPreset {
            name: "p".into(),
            container: container.into(),
            video_codec: v.into(),
            audio_codec: a.into(),
            resolution: None,
            fps: None,
            audio_tracks: TrackSelection::All,
            subtitle_tracks: SubtitleSelection::Burned,
        }
    }

    #[test]
    fn valid_and_invalid_pairings() {
        assert!(preset("mp4", "h264", "aac").validate().is_ok());
        assert!(preset("mkv", "h265", "flac").validate().is_ok());
        assert!(preset("webm", "vp9", "opus").validate().is_ok());

        for bad in [
            preset("avi", "h264", "aac"),
            preset("webm", "h264", "opus"),
            preset("mp4", "h264", "flac"),
        ] {
            let e = bad.validate().expect_err("must reject");
            let msg = e.user_message();
            assert!(msg.starts_with("export preset 'p'"), "{msg}");
        }
    }

    #[test]
    fn embedded_subtitles_are_refused_for_mp4() {
        let mut p = preset("mp4", "h264", "aac");
        p.subtitle_tracks = SubtitleSelection::Embedded;
        assert!(p.validate().is_err());
        p.container = "mkv".into();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn codecs_become_ffmpeg_encoder_names() {
        let mut p = preset("mkv", "h264", "opus");
        p.resolution = Some(Resolution {
            width: 1280,
            height: 720,
        });
        let s = p.render_settings(Resolution::HD_1080, Fps::FPS_60);
        assert_eq!(s.video_codec, "libx264");
        assert_eq!(s.audio_codec, "libopus");
        assert_eq!(s.resolution.width, 1280);
        assert_eq!(s.fps, Fps::FPS_60);
    }

    #[test]
    fn an_unspecified_geometry_falls_back_to_the_timeline() {
        let s = preset("mkv", "h264", "aac").render_settings(Resolution::HD_1080, Fps::FPS_60);
        assert_eq!(s.resolution, Resolution::HD_1080);
        assert_eq!(s.fps, Fps::FPS_60);
    }

    #[test]
    fn resolutions_parse() {
        assert_eq!(
            parse_resolution("1920x1080"),
            Some(Resolution {
                width: 1920,
                height: 1080
            })
        );
        assert_eq!(parse_resolution("1080p"), None);
    }
}
