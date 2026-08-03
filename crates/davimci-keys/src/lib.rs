//! Key sequence parser and mode state machine (plan.md Phase 4, spec §3,
//! §4, §6, §11).
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
//!   the undo log, per spec §3.2.1.
//!
//! Known simplifications, tracked against later phases rather than spec
//! gaps: `i`/`a`/`r` need the Phase 5 media picker and currently report
//! [`engine::Outcome::NotImplemented`]; `gx`/`dax` wait on Phase 9f
//! transitions the same way; `<`/`>` jump-point edge trims are not yet
//! wired to a command. Visual-mode track-object narrowing (typing `it`/`at`
//! *while* a selection is live) is not implemented - operators in a
//! `VISUAL*` mode act on the selection as a whole.

pub mod action;
pub mod engine;
pub mod error;
pub mod key;
pub mod keymap;
pub mod mode;
pub mod parser;

#[cfg(test)]
mod tests;

pub use action::{Action, ArgKind, LeafAction, Operator, Target, TransportPolicy, ZoomIntent};
pub use engine::{Engine, Feed, MediaIntent, Outcome, TransportCmd};
pub use error::KeysError;
pub use key::{Key, Named};
pub use keymap::{Keymap, Lookup};
pub use mode::{Anchor, Mode, ModeChanged, ModeState, VisualSelection};
pub use parser::{Parser, Step};
