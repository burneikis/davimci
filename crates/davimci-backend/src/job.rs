//! Render jobs and their progress reporting.

use std::path::PathBuf;

use davimci_core::{Fps, Frame, Resolution};

/// Encoder settings for one export. Presets (Phase 8b) build these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSettings {
    pub resolution: Resolution,
    pub fps: Fps,
    /// An ffmpeg *encoder* name, never a marketing name (spec §10.3).
    pub video_codec: String,
    pub audio_codec: String,
    /// Container extension, e.g. `mkv`, `mp4`.
    pub container: String,
    /// Extra backend properties, passed through verbatim.
    pub extra: Vec<(String, String)>,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            resolution: Resolution::HD_1080,
            fps: Fps::FPS_60,
            video_codec: "libx264".into(),
            audio_codec: "aac".into(),
            container: "mkv".into(),
            extra: Vec::new(),
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
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            return match self.state {
                RenderState::Done => 1.0,
                _ => 0.0,
            };
        }
        (self.rendered as f32 / self.total as f32).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
