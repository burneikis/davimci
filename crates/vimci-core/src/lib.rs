//! vimci timeline model.
//!
//! Engine-agnostic by rule (spec §10.1): this crate must compile and be fully
//! testable with no render backend present, and must never reference MLT.

pub mod error;
pub mod time;

pub use error::{Classify, CoreError, ErrorClass, Notice};
pub use time::{Fps, Frame, Resolution, TimelineProps};
