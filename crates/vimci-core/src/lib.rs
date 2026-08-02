//! vimci timeline model.
//!
//! Engine-agnostic by rule (spec §10.1): this crate must compile and be fully
//! testable with no render backend present, and must never reference MLT.

pub mod clip;
pub mod edit;
pub mod error;
pub mod id;
pub mod time;
pub mod timeline;
pub mod track;
pub mod trim;

#[cfg(test)]
mod props;
#[cfg(test)]
mod snapshots;
#[cfg(test)]
pub(crate) mod testing;

pub use clip::{Clip, ClipProps, MediaRef, Transform};
pub use error::{Classify, CoreError, ErrorClass, Notice};
pub use id::{ClipId, GroupId, TrackId};
pub use time::{Fps, Frame, Resolution, TimelineProps};
pub use timeline::{Mark, Marker, Playhead, Register, Timeline};
pub use track::{Track, TrackKind};
pub use trim::Edge;
