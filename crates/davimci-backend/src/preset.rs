//! Export presets.
//!
//! A preset names a *codec* - `h264`, `vp9`, `opus` - and never an ffmpeg
//! encoder. The mapping from one to the other lives here and nowhere else, so
//! A user writes what they mean and the editor picks the
//! encoder.
//!
//! Container/codec pairings are validated where a preset is defined, not
//! where it runs. A misspelled container is a user error, and finding it out
//! after a forty-minute render is not acceptable.
//!
//! This module is pure data. It knows nothing about MLT, so the whole preset
//! system is testable with no media, no backend, and no window.

use std::collections::BTreeMap;

use davimci_core::{Fps, Resolution};

use crate::job::RenderSettings;

/// A video codec a preset may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VideoCodec {
    H264,
    H265,
    Vp9,
    ProRes,
}

impl VideoCodec {
    /// The name a user writes in a preset.
    #[must_use]
    pub fn spec_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "h265",
            Self::Vp9 => "vp9",
            Self::ProRes => "prores",
        }
    }

    /// The ffmpeg encoder this codec maps to.
    #[must_use]
    pub fn encoder(self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::Vp9 => "libvpx-vp9",
            Self::ProRes => "prores_ks",
        }
    }

    /// The VAAPI encoder for this codec, if one exists.
    ///
    /// Same codec, so a hardware encode still satisfies a preset that named
    /// `h264`. A codec with no entry here cannot be accelerated at all,
    /// which is why a preset that requires hardware is refused when it names
    /// one - refusing at definition time rather than after a long render.
    #[must_use]
    pub fn hardware_encoder(self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("h264_vaapi"),
            Self::H265 => Some("hevc_vaapi"),
            Self::Vp9 => Some("vp9_vaapi"),
            // Intra-only and quality-critical: no VAAPI encoder produces a
            // ProRes stream, so this is a refusal, not a gap.
            Self::ProRes => None,
        }
    }

    /// Parse a codec name, rejecting encoder names on purpose: accepting
    /// `libx264` here would make section 10.3 a suggestion rather than a rule.
    pub fn parse(name: &str) -> Result<Self, PresetError> {
        match name {
            "h264" => Ok(Self::H264),
            "h265" | "hevc" => Ok(Self::H265),
            "vp9" => Ok(Self::Vp9),
            "prores" => Ok(Self::ProRes),
            other => Err(PresetError::UnknownVideoCodec {
                name: other.to_string(),
            }),
        }
    }
}

/// An audio codec a preset may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AudioCodec {
    Aac,
    Opus,
    Flac,
    Pcm,
}

impl AudioCodec {
    #[must_use]
    pub fn spec_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Opus => "opus",
            Self::Flac => "flac",
            Self::Pcm => "pcm",
        }
    }

    #[must_use]
    pub fn encoder(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Opus => "libopus",
            Self::Flac => "flac",
            Self::Pcm => "pcm_s16le",
        }
    }

    pub fn parse(name: &str) -> Result<Self, PresetError> {
        match name {
            "aac" => Ok(Self::Aac),
            "opus" => Ok(Self::Opus),
            "flac" => Ok(Self::Flac),
            "pcm" => Ok(Self::Pcm),
            other => Err(PresetError::UnknownAudioCodec {
                name: other.to_string(),
            }),
        }
    }
}

/// A container a preset may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Container {
    Mkv,
    Mp4,
    WebM,
    Mov,
}

impl Container {
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mkv => "mkv",
            Self::Mp4 => "mp4",
            Self::WebM => "webm",
            Self::Mov => "mov",
        }
    }

    pub fn parse(name: &str) -> Result<Self, PresetError> {
        match name {
            "mkv" | "matroska" => Ok(Self::Mkv),
            "mp4" => Ok(Self::Mp4),
            "webm" => Ok(Self::WebM),
            "mov" => Ok(Self::Mov),
            other => Err(PresetError::UnknownContainer {
                name: other.to_string(),
            }),
        }
    }

    /// Whether this container can legally carry these codecs.
    ///
    /// Only Matroska keeps every audio track as a separate stream, which is
    /// why multi-track audio export is an MKV feature here.
    #[must_use]
    pub fn accepts(self, video: VideoCodec, audio: AudioCodec) -> bool {
        match self {
            // Matroska carries essentially anything.
            Self::Mkv => true,
            Self::Mp4 => {
                matches!(
                    video,
                    VideoCodec::H264 | VideoCodec::H265 | VideoCodec::ProRes
                ) && matches!(audio, AudioCodec::Aac | AudioCodec::Opus | AudioCodec::Flac)
            }
            Self::WebM => video == VideoCodec::Vp9 && audio == AudioCodec::Opus,
            Self::Mov => {
                matches!(
                    video,
                    VideoCodec::H264 | VideoCodec::H265 | VideoCodec::ProRes
                ) && matches!(audio, AudioCodec::Aac | AudioCodec::Pcm)
            }
        }
    }

    /// True when every audio track survives as its own stream.
    #[must_use]
    pub fn keeps_audio_tracks_separate(self) -> bool {
        self == Self::Mkv
    }
}

/// Which audio tracks reach the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TrackSelection {
    /// Every audio track, each as its own stream where the container allows.
    #[default]
    All,
    None,
    /// Named tracks, e.g. `A1`, `A3`.
    Named(Vec<String>),
}

/// What happens to subtitle tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SubtitleMode {
    Burned,
    Embedded,
    Sidecar,
    #[default]
    None,
}

impl SubtitleMode {
    pub fn parse(name: &str) -> Result<Self, PresetError> {
        match name {
            "burned" => Ok(Self::Burned),
            "embedded" => Ok(Self::Embedded),
            "sidecar" => Ok(Self::Sidecar),
            "none" => Ok(Self::None),
            other => Err(PresetError::UnknownSubtitleMode {
                name: other.to_string(),
            }),
        }
    }
}

/// One named export preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: String,
    pub container: Container,
    pub video: VideoCodec,
    pub audio: AudioCodec,
    /// `None` means "the timeline's", resolved at render time.
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub audio_tracks: TrackSelection,
    pub subtitles: SubtitleMode,
    /// Backend properties passed through verbatim, e.g. `crf`.
    pub extra: Vec<(String, String)>,
    /// Whether this preset *requires* a hardware encoder. Opt-in per preset,
    /// and binding: an export that cannot meet it is refused rather than
    /// re-encoded in software at a quality nobody asked for.
    pub hardware: bool,
}

impl Preset {
    /// Define a preset, validating the container/codec pairing now rather
    /// than after a long render.
    pub fn new(
        name: impl Into<String>,
        container: Container,
        video: VideoCodec,
        audio: AudioCodec,
    ) -> Result<Self, PresetError> {
        let name = name.into();
        if !container.accepts(video, audio) {
            return Err(PresetError::IncompatiblePairing {
                preset: name,
                container: container.extension().to_string(),
                video: video.spec_name().to_string(),
                audio: audio.spec_name().to_string(),
            });
        }
        Ok(Self {
            name,
            container,
            video,
            audio,
            resolution: None,
            fps: None,
            audio_tracks: TrackSelection::All,
            subtitles: SubtitleMode::None,
            extra: Vec::new(),
            hardware: false,
        })
    }

    /// Require a hardware encoder for this preset.
    ///
    /// Validated here, where the preset is defined: a codec with no hardware
    /// encoder at all is a preset that could never run, and finding that out
    /// at export time would be finding it out too late.
    pub fn require_hardware(mut self) -> Result<Self, PresetError> {
        if self.video.hardware_encoder().is_none() {
            return Err(PresetError::NoHardwareEncoder {
                preset: self.name,
                video: self.video.spec_name().to_string(),
            });
        }
        self.hardware = true;
        Ok(self)
    }

    /// Resolve to encoder settings against a timeline's properties. This is
    /// where `resolution = nil` becomes the timeline's resolution.
    #[must_use]
    pub fn settings(&self, timeline_res: Resolution, timeline_fps: Fps) -> RenderSettings {
        RenderSettings {
            resolution: self.resolution.unwrap_or(timeline_res),
            fps: self.fps.unwrap_or(timeline_fps),
            video_codec: self.video.encoder().to_string(),
            audio_codec: self.audio.encoder().to_string(),
            container: self.container.extension().to_string(),
            // A preset can only ask; whether the sources can actually be
            // routed is decided against the timeline at export time.
            separate_audio_tracks: self.container.keeps_audio_tracks_separate()
                && self.audio_tracks != TrackSelection::None,
            // Only `burned` paints the text into the picture; the other
            // modes keep it out of the graph entirely.
            burn_subtitles: self.subtitles == SubtitleMode::Burned,
            extra: self.extra.clone(),
            hardware: if self.hardware {
                crate::accel::HardwareEncode::Required
            } else {
                crate::accel::HardwareEncode::Off
            },
        }
    }

    /// A one-line description for `:presets`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} - {}/{}+{}",
            self.name,
            self.container.extension(),
            self.video.spec_name(),
            self.audio.spec_name()
        )
    }
}

/// Every preset the editor knows, builtin and user-defined.
#[derive(Debug, Clone)]
pub struct PresetRegistry {
    presets: BTreeMap<String, Preset>,
}

impl Default for PresetRegistry {
    fn default() -> Self {
        Self::with_fallback()
    }
}

impl PresetRegistry {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            presets: BTreeMap::new(),
        }
    }

    /// The one preset that exists before any config runs.
    ///
    /// A *catalogue* of presets is registration data and belongs to a
    /// plugin, the same as the transition catalogue. Being able to export at
    /// all is not, so exactly one survives here: `mkv`, because it is the
    /// only container that keeps every audio track separate, which is what
    /// an export of a multi-track timeline has to do.
    #[must_use]
    pub fn with_fallback() -> Self {
        let mut r = Self::empty();
        // Validated by the same rule as a user preset; if it is wrong the
        // test suite says so rather than a user finding out after a render.
        if let Ok(p) = Preset::new("mkv", Container::Mkv, VideoCodec::H264, AudioCodec::Flac) {
            r.presets.insert(p.name.clone(), p);
        }
        r
    }

    /// Add or replace a preset. Later definitions win, so a user config can
    /// override a builtin by name.
    pub fn define(&mut self, preset: Preset) {
        self.presets.insert(preset.name.clone(), preset);
    }

    pub fn get(&self, name: &str) -> Result<&Preset, PresetError> {
        self.presets.get(name).ok_or_else(|| PresetError::Unknown {
            name: name.to_string(),
            known: self.names().join(", "),
        })
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.presets.keys().map(String::as_str).collect()
    }

    pub fn all(&self) -> impl Iterator<Item = &Preset> {
        self.presets.values()
    }

    /// The preset to use when the user named none: inferred from the output
    /// file's extension, falling back to `mkv`.
    #[must_use]
    pub fn for_extension(&self, ext: &str) -> &Preset {
        let want = ext.to_ascii_lowercase();
        self.presets
            .values()
            .find(|p| p.container.extension() == want)
            .or_else(|| self.presets.get("mkv"))
            .unwrap_or_else(|| {
                // The registry is never empty in practice; this keeps the
                // function total without an unwrap.
                #[allow(clippy::missing_panics_doc)]
                self.presets
                    .values()
                    .next()
                    .unwrap_or_else(|| unreachable!("a registry with no presets"))
            })
    }
}

/// Everything that can be wrong with a preset. Each message is a complete
/// user-facing sentence (Phase 0).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresetError {
    #[error("there is no export preset called '{name}'; the ones that exist are: {known}")]
    Unknown { name: String, known: String },

    #[error("'{name}' is not a video codec davimci knows; try h264, h265, vp9, or prores")]
    UnknownVideoCodec { name: String },

    #[error("'{name}' is not an audio codec davimci knows; try aac, opus, flac, or pcm")]
    UnknownAudioCodec { name: String },

    #[error("'{name}' is not a container davimci knows; try mkv, mp4, webm, or mov")]
    UnknownContainer { name: String },

    #[error("'{name}' is not a subtitle mode; try burned, embedded, sidecar, or none")]
    UnknownSubtitleMode { name: String },

    #[error(
        "the preset '{preset}' asks for {video} video and {audio} audio in a {container} file, \
         which that container cannot carry"
    )]
    IncompatiblePairing {
        preset: String,
        container: String,
        video: String,
        audio: String,
    },

    #[error(
        "the preset '{preset}' requires a hardware encoder, but no hardware encoder produces \
         {video}"
    )]
    NoHardwareEncoder { preset: String, video: String },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_preset_names_codecs_and_resolves_to_encoders() {
        let p = Preset::new("x", Container::Mkv, VideoCodec::H264, AudioCodec::Flac).unwrap();
        let s = p.settings(Resolution::HD_1080, Fps::FPS_60);
        // The user wrote h264; the backend is handed libx264.
        assert_eq!(s.video_codec, "libx264");
        assert_eq!(s.audio_codec, "flac");
    }

    #[test]
    fn an_encoder_name_is_not_a_codec_name() {
        // Accepting this would make the proxy rule advisory.
        assert!(VideoCodec::parse("libx264").is_err());
    }

    #[test]
    fn an_impossible_pairing_is_refused_when_the_preset_is_defined() {
        // vp9 in mp4 - caught now, not after the render.
        let e = Preset::new("bad", Container::Mp4, VideoCodec::Vp9, AudioCodec::Aac).unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("cannot carry"), "{msg}");
        assert!(msg.starts_with("the preset 'bad'"), "{msg}");
    }

    #[test]
    fn the_fallback_preset_is_a_legal_pairing_and_stands_alone() {
        let r = PresetRegistry::with_fallback();
        assert_eq!(r.names(), ["mkv"], "the catalogue belongs to a plugin");
        for p in r.all() {
            assert!(
                p.container.accepts(p.video, p.audio),
                "the fallback '{}' is not a legal pairing",
                p.name
            );
        }
    }

    #[test]
    fn resolution_defaults_to_the_timelines() {
        let p = Preset::new("x", Container::Mkv, VideoCodec::H264, AudioCodec::Aac).unwrap();
        let odd = Resolution {
            width: 1234,
            height: 567,
        };
        assert_eq!(p.settings(odd, Fps::FPS_30).resolution, odd);
    }

    #[test]
    fn a_user_preset_may_override_a_builtin_by_name() {
        let mut r = PresetRegistry::with_fallback();
        let mine = Preset::new("mkv", Container::Mkv, VideoCodec::H265, AudioCodec::Opus).unwrap();
        r.define(mine);
        assert_eq!(r.get("mkv").unwrap().video, VideoCodec::H265);
    }

    #[test]
    fn an_unknown_preset_lists_the_ones_that_exist() {
        let r = PresetRegistry::with_fallback();
        let msg = r.get("youtube").unwrap_err().to_string();
        assert!(msg.contains("no export preset called 'youtube'"), "{msg}");
        assert!(msg.contains("mkv"), "{msg}");
    }

    #[test]
    fn only_matroska_keeps_audio_tracks_separate() {
        assert!(Container::Mkv.keeps_audio_tracks_separate());
        assert!(!Container::Mp4.keeps_audio_tracks_separate());
    }

    #[test]
    fn an_extension_picks_a_matching_preset() {
        let mut r = PresetRegistry::with_fallback();
        // As the bundled `presets` plugin registers it.
        r.define(Preset::new("webm", Container::WebM, VideoCodec::Vp9, AudioCodec::Opus).unwrap());
        assert_eq!(r.for_extension("webm").container, Container::WebM);
        // Unknown extensions fall back rather than failing the export, and
        // so does every extension in a session with no catalogue.
        assert_eq!(r.for_extension("wat").container, Container::Mkv);
        assert_eq!(
            PresetRegistry::with_fallback()
                .for_extension("webm")
                .container,
            Container::Mkv
        );
    }
}
