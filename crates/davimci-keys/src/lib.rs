//! Key sequence parser and mode state machine.
//!
//! Three layers, kept deliberately separate:
//!
//! - [`key`] / [`keymap`] - key tokens and the literal-sequence table
//!   (defaults plus user overrides, longest match wins).
//! - [`parser`] - the compositional grammar (`[count] [register] operator
//!   [count] motion|textobject`) as a pure state machine. It never touches a
//!   [`davimci_core::Timeline`], which is what makes golden key-string tests
//!   possible with no fixture.
//! - [`engine`] - gives the parsed [`action::Action`] meaning against a live
//!   [`davimci_cmd::Session`], using [`davimci_motion`] to resolve targets and
//!   [`davimci_cmd`] to apply them. Transport is dispatched separately from
//!   the undo log.
//!
//! Two known gaps: `<`/`>` jump-point edge trims parse but have no command
//! yet, and typing `it`/`at` while a visual selection is live does not narrow
//! it - operators in a `VISUAL*` mode act on the selection as a whole.

pub mod action;
pub mod docs;
pub mod engine;
pub mod error;
pub mod key;
pub mod keymap;
pub mod mode;
pub mod parser;

#[cfg(test)]
mod tests;

pub use action::{
    Action, ArgKind, CenterIntent, LeafAction, Operator, Target, TransportPolicy, ZoomIntent,
};
pub use engine::{Engine, Feed, MediaIntent, Outcome, TransportCmd};
pub use error::KeysError;
pub use key::{Key, Named};
pub use keymap::{Keymap, Lookup};
pub use mode::{Anchor, Mode, ModeChanged, ModeState, VisualSelection, VisualStart};
pub use parser::{Parser, Step};
