//! Render jobs and their progress reporting.

use std::path::PathBuf;

use davimci_core::{Fps, Frame, Resolution};

/// Encoder settings for one export. Presets (Phase 8b) build these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSettings {
    pub resolution: Resolution,
    pub fps: Fps,
    /// An ffmpeg *encoder* name, never a marketing name.
    pub video_codec: String,
    pub audio_codec: String,
    /// Container extension, e.g. `mkv`, `mp4`.
    pub container: String,
    /// Whether each audio track gets its own stream in the file.
    /// Only some containers can carry that, and only some sources can be
    /// routed, so this is decided before the render rather than during it.
    pub separate_audio_tracks: bool,
    /// Whether text tracks are composited into the picture. False
    /// for the sidecar and embedded modes, where the subtitles travel as
    /// their own file or stream instead of being painted on.
    pub burn_subtitles: bool,
    /// Extra backend properties, passed through verbatim.
    pub extra: Vec<(String, String)>,
    /// What this export asks of a hardware encoder. The backend substitutes
    /// the encoder or refuses the job; it never downgrades a requirement.
    pub hardware: crate::accel::HardwareEncode,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::HD_1080,
            fps: Fps::FPS_60,
            video_codec: "libx264".into(),
            audio_codec: "aac".into(),
            container: "mkv".into(),
            separate_audio_tracks: false,
            burn_subtitles: true,
            extra: Vec::new(),
            hardware: crate::accel::HardwareEncode::Off,
        }
    }
}

/// One export request: what to render, where to, and over what range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderJob {
    pub output: PathBuf,
    pub settings: RenderSettings,
    /// Inclusive start, exclusive end. `None` means the whole timeline.
    pub range: Option<(Frame, Frame)>,
}

impl RenderJob {
    #[must_use]
    pub fn new(output: impl Into<PathBuf>, settings: RenderSettings) -> Self {
        Self {
            output: output.into(),
            settings,
            range: None,
        }
    }
}

/// Where a render currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderState {
    Idle,
    Running,
    Done,
    Cancelled,
    /// Carries a user-facing sentence, per Phase 0.
    Failed(String),
}

impl RenderState {
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Idle | Self::Running)
    }
}

/// Progress snapshot, safe to poll from the status line every frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderProgress {
    pub state: RenderState,
    pub rendered: u64,
    pub total: u64,
}

impl RenderProgress {
    #[must_use]
    pub fn idle() -> Self {
        Self {
            state: RenderState::Idle,
            rendered: 0,
            total: 0,
        }
    }

    /// Completion in `0.0..=1.0`. An unknown total reports zero rather than
    /// dividing by it.
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "a progress fraction is wanted to a percent, not to a frame"
    )]
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return match self.state {
                RenderState::Done => 1.0,
                _ => 0.0,
            };
        }
        // Frame counts far below the f32 mantissa in any real render, and a
        // fraction is wanted to a percent, not to a frame.
        let done = u32::try_from(self.rendered).unwrap_or(u32::MAX);
        let total = u32::try_from(self.total).unwrap_or(u32::MAX);
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(
    clippy::float_cmp,
    reason = "the values under test are set exactly, so exact equality is the assertion"
)]
mod tests {
    use super::*;

    #[test]
    fn fraction_is_bounded_and_total_free() {
        let mut p = RenderProgress::idle();
        assert_eq!(p.fraction(), 0.0);
        p.state = RenderState::Done;
        assert_eq!(p.fraction(), 1.0);
        p.total = 100;
        p.rendered = 250;
        assert_eq!(p.fraction(), 1.0);
        p.rendered = 25;
        assert!((p.fraction() - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn terminal_states_are_exactly_the_finished_ones() {
        assert!(!RenderState::Idle.is_terminal());
        assert!(!RenderState::Running.is_terminal());
        assert!(RenderState::Done.is_terminal());
        assert!(RenderState::Cancelled.is_terminal());
        assert!(RenderState::Failed("x".into()).is_terminal());
    }
}

/// A transition type a config registered.
///
/// Named in backend terms and nothing more: `service` is whatever the render
/// engine calls the effect, and `props` are passed to it verbatim. The layer
/// that writes one of these never needs to know which engine is behind the
/// trait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionDef {
    pub name: String,
    pub service: String,
    pub props: Vec<(String, String)>,
}
