//! One video path, two hosts.
//!
//! This crate is the anti-duplication keystone: the GUI and the TUI hand
//! frames to the same [`Presenter`], which paces them against the backend's
//! audio clock, letterboxes them, and produces a composited RGBA image plus
//! an overlay *model*. A host decides where those pixels go - a `wgpu`
//! texture, a terminal's cells - and nothing else.
//!
//! Composition is software and integral on purpose. It is what lets the
//! headless and windowed paths be compared byte for byte, and what makes the
//! host-parity test (`Embedded` and `Detached` produce identical video pixels)
//! a real assertion rather than a formality. A GPU upload path is a host
//! detail that must reproduce these pixels, not redefine them.
//!
//! Text is never rasterised here: overlays are described (timecode string,
//! safe-area rectangles) and drawn by the host's own text stack, so one
//! timecode cannot look like two.

pub mod error;
pub mod fit;
pub mod headless;
pub mod overlay;
pub mod pacing;
pub mod presenter;

pub use error::PresentError;
pub use fit::{Quad, letterbox};
pub use headless::HeadlessPresenter;
pub use overlay::{Overlay, OverlayConfig, OverlayRect, OverlayRectKind, timecode};
pub use pacing::{Pace, PaceStats, Pacer};
pub use presenter::{Host, Presentation, Presenter};
