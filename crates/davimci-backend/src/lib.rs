//! The `RenderBackend` boundary.
//!
//! MLT sits behind this trait so it can be replaced without touching the
//! editor core. Nothing here may reference MLT types, and nothing here does
//! any decoding: this crate is the interface plus a deterministic
//! [`MockBackend`] that every upstream test runs against.
//!
//! The preview contract is frame pull, not a backend-owned window: the
//! backend hands out RGBA buffers and `davimci-present` puts them on screen,
//! which is what lets overlays exist and lets the GUI and TUI share one video
//! path.

pub mod accel;
pub mod error;
pub mod frame;
pub mod job;
pub mod mock;
pub mod preset;

use std::path::Path;

use davimci_core::{Fps, Frame, Resolution, Timeline};

pub use accel::{AccelerationStatus, DecodePolicy};
pub use error::{BackendError, Result};
pub use frame::{PreviewScale, VideoFrame};
pub use job::{RenderJob, RenderProgress, RenderSettings, RenderState, TransitionDef};
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
    /// and ripple are playlist mutations.
    fn set_timeline(&mut self, timeline: &Timeline) -> Result<()>;

    /// Move the playhead. Frame-exact: no nearest-keyframe behaviour.
    fn seek(&mut self, frame: Frame) -> Result<()>;

    /// Pull one frame at an explicit position, for scrubbing and tests.
    fn frame_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame>;

    /// Pull one frame for something other than the playhead - a timeline
    /// thumbnail, say.
    ///
    /// Distinct from [`RenderBackend::frame_at`] because a backend may read
    /// the sequence of playhead requests to tell a backward step from a
    /// forward one, and an interleaved thumbnail must not be mistaken for the
    /// user moving.
    fn thumbnail_at(&mut self, frame: Frame, scale: PreviewScale) -> Result<VideoFrame> {
        self.frame_at(frame, scale)
    }

    /// Start realtime playback from `from`, with audio going to the system
    /// output. Video is *not* displayed by the backend.
    fn preview_start(&mut self, from: Frame, scale: PreviewScale) -> Result<()>;

    fn preview_stop(&mut self) -> Result<()>;

    fn is_previewing(&self) -> bool;

    /// Whether this backend can play at a rate other than 1x.
    ///
    /// Asked rather than assumed: a backend without it shuttles by stepping
    /// the playhead, which is a different feature with the same key
    ///.
    fn supports_varispeed(&self) -> bool {
        false
    }

    /// Whether a *negative* rate plays rather than stalls.
    ///
    /// Separate from [`RenderBackend::supports_varispeed`] because running a
    /// graph backwards is a different capability from running it fast: a
    /// backend that cannot do it must be stepped instead, since a stalled
    /// reverse preview freezes the picture and strands the playhead.
    fn supports_reverse_varispeed(&self) -> bool {
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

    /// Add a transition type. A backend that has no extensible
    /// transitions refuses, and the type keeps degrading to a dissolve.
    fn register_transition(&mut self, def: TransitionDef) -> Result<()> {
        let _ = def;
        Err(BackendError::Unavailable {
            reason: "this backend has no transition registry".into(),
        })
    }

    /// Every transition type this backend can render, built-in or
    /// registered.
    fn transition_names(&self) -> Vec<String> {
        Vec::new()
    }

    /// Ask for hardware decode, or ask to stay on the CPU.
    ///
    /// Infallible by construction: an unusable device is a recoverable
    /// condition, so the backend degrades to software and says so in the
    /// status it returns rather than failing the call.
    fn set_decode_policy(&mut self, policy: DecodePolicy) -> AccelerationStatus {
        let _ = policy;
        AccelerationStatus::unsupported()
    }

    /// What acceleration is in use right now, for a health report.
    fn acceleration(&self) -> AccelerationStatus {
        AccelerationStatus::unsupported()
    }

    /// Begin an export. Progress is polled; the call itself does not block.
    fn render(&mut self, job: RenderJob) -> Result<()>;

    fn progress(&self) -> RenderProgress;

    fn cancel_render(&mut self) -> Result<()>;
}
