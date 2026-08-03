//! Frontend-agnostic view state and event loop (plan.md Phase 9a).
//!
//! Built before any frontend so no frontend can invent its own. The rule this
//! crate exists to enforce: no frontend contains view logic. Zoom,
//! scroll-follow, ruler ticks, the mode line, message and job reporting, and
//! the meaning of a key all live here; a frontend polls events, reports its
//! size, and draws a [`ViewState`].
//!
//! Nothing here does any I/O, owns a window, or touches a render backend, so
//! the whole crate is unit-testable with no display present.

pub mod app;
pub mod browse;
pub mod cmdline;
pub mod error;
pub mod frontend;
pub mod job;
pub mod message;
pub mod plugin;
pub mod thumbnail;
pub mod view;
pub mod viewport;
pub mod waveform;

#[cfg(any(test, feature = "testing"))]
pub mod fixtures;

pub use app::{App, Host, NullHost};
pub use browse::{BrowseEntry, is_media, list_dir};
pub use cmdline::{CommandKey, CommandLine, CommandLineEvent, default_candidates};
pub use error::AppError;
pub use frontend::{Event, Frontend, Response, Surface};
pub use job::{Job, JobList, JobState, JobUpdate};
pub use message::{Message, MessageQueue, Severity};
pub use plugin::PluginEffects;
pub use thumbnail::{Thumbnail, ThumbnailRequest, Thumbnails};
pub use view::{
    ClipView, CommandLineView, PlayheadView, SelectionView, Tick, TrackView, ViewInputs, ViewState,
};
pub use viewport::Viewport;
pub use waveform::{Waveform, Waveforms};
