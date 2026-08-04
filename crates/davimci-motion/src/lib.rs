//! Motions, jump points, and text objects.
//!
//! Everything here is a *query*: a motion or object reads the timeline and
//! reports where the playhead should go or which range and tracks a verb
//! should act on. Nothing in this crate mutates, and nothing in it touches
//! media or a render backend - predicate motions reach analysis through the
//! [`predicate::PredicateIndex`] trait, which Phase 5 implements.

pub mod error;
pub mod jump;
pub mod motion;
pub mod object;
pub mod predicate;
pub mod target;

#[cfg(test)]
mod props;
#[cfg(test)]
mod tests;

pub use error::MotionError;
pub use jump::{
    BASE_FRAMES_PER_COLUMN, JumpConfig, JumpPointCache, JumpPoints, JumpSources, Zoom,
    frames_per_column,
};
pub use motion::{BuiltinMotion, Motion, MotionCtx};
pub use object::{Object, TextObject};
pub use predicate::{Answer, NoAnalysis, Predicate, PredicateIndex};
pub use target::{Direction, Position, Resolved, Scope, TimeRange};
