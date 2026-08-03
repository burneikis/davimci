//! The `RenderBackend` boundary (plan.md Phase 6, spec §10.1).
//!
//! MLT sits behind this trait so it can be replaced without touching the
//! editor core. Nothing here may reference MLT types, and nothing here does
//! any decoding: this crate is the interface plus a deterministic
//! [`MockBackend`] that every upstream test runs against.
//!
//! The preview contract is **frame pull, not a backend-owned window**: the
//! backend hands out RGBA buffers and `davimci-present` puts them on screen,
//! which is what lets overlays exist and lets the GUI and TUI share one video
//! path.

pub mod error;
pub mod frame;
pub mod job;
pub mod mock;
pub mod preset;

use std::path::Path;

use davimci_core::{Fps, Frame, Resolution, Timeline};

pub use error::{BackendError, Result};
pub use frame::{PreviewScale, VideoFrame};
pub use job::{RenderJob, RenderProgress, RenderSettings, RenderState};
pub use mock::MockBackend;
pub use preset::{
    AudioCodec, Container, Preset, PresetError, PresetRegistry, SubtitleMode, TrackSelection,
    VideoCodec,
};

/// What the backend can tell us about a media file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub path: String,
    pub has_video: bool,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    /// Length in *source* frames; conform to timeline frames at the edge.
    pub frames: u64,
    pub audio_streams: usize,
    pub sample_rate: Option<u32>,
}

/// The render/preview engine behind the editor.
///
/// Implementations are single-threaded from the caller's point of view: the
/// app owns one backend and drives it from the event loop. Long work
/// ([`RenderBackend::render`]) is expected to run off-thread internally and be
/// observed through [`RenderBackend::progress`].
pub trait RenderBackend {
    /// Inspect a media file without adding it to the timeline.
    fn probe(&mut self, path: &Path) -> Result<SourceInfo>;

    /// Project the timeline onto the render graph.
    ///
    /// Called after every committed edit. Implementations should patch the
    /// existing graph where they can rather than rebuilding it, since split
    /// and ripple are playlist mutations (spec §10.1).
    fn set_timeline(&mut self, timeline: &Timeline) -> Result<()>;

    /// Move the playhead. Frame-exact: no nearest-keyframe behaviour.
    fn seek(&mut self, frame: Frame) -> Result<()>;

    /// Pull one frame at an explicit position, for scrubbing and tests.
    fn frame_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame>;

    /// Start realtime playback from `from`, with audio going to the system
    /// output. Video is *not* displayed by the backend.
    fn preview_start(&mut self, from: Frame, scale: PreviewScale) -> Result<()>;

    fn preview_stop(&mut self) -> Result<()>;

    fn is_previewing(&self) -> bool;

    /// Whether this backend can play at a rate other than 1x.
    ///
    /// Asked rather than assumed: a backend without it shuttles by stepping
    /// the playhead, which is a different feature with the same key
    /// (spec §3.2.1).
    fn supports_varispeed(&self) -> bool {
        false
    }

    /// Set the playback rate: `1.0` is normal, `2.0` double speed, negative
    /// plays backwards. Only meaningful while previewing.
    fn set_rate(&mut self, rate: f64) -> Result<()> {
        let _ = rate;
        Err(BackendError::Unavailable {
            reason: "this backend cannot play at other than normal speed".into(),
        })
    }

    /// The next frame due for presentation, or `None` if none is ready yet.
    fn next_preview_frame(&mut self) -> Result<Option<VideoFrame>>;

    /// Master clock position, in timeline frames. Audio is the master clock,
    /// so this is what the presenter paces against.
    fn audio_clock_position(&self) -> Option<Frame>;

    /// Begin an export. Progress is polled; the call itself does not block.
    fn render(&mut self, job: RenderJob) -> Result<()>;

    fn progress(&self) -> RenderProgress;

    fn cancel_render(&mut self) -> Result<()>;
}
