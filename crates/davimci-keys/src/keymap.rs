//! The keymap table: literal key sequences to [`LeafAction`]s, with user
//! overrides resolved over the defaults (plan.md Phase 4).
//!
//! Counts, registers, operator targets, and text objects are *not* entries
//! here - they compose, so [`crate::parser::Parser`] handles them. What
//! lives in the table is everything that is a fixed sequence: bare motions,
//! operator triggers, and standalone commands, including the ambiguous
//! `g`-prefixed and `<Space>`-leader families (spec §3.2.1, §11).

use std::collections::HashMap;

use crate::action::{Action, ArgKind, LeafAction, Operator};
use crate::key::Key;
use crate::mode::Mode;
use davimci_motion::{BuiltinMotion, Direction};

/// The result of matching a key buffer against the table.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// The buffer matches nothing bound, and extends nothing bound either.
    NoMatch,
    /// The buffer is a strict prefix of one or more bindings, and is not
    /// itself bound: keep collecting keys.
    Pending,
    /// The buffer is itself bound *and* a strict prefix of a longer binding:
    /// keep collecting, but `leaf` is what a timeout should resolve to -
    /// "longest match wins" only when a longer sequence is actually typed.
    PendingWithFallback(LeafAction),
    /// The buffer is bound and nothing longer shares its prefix.
    Match(LeafAction),
}

/// Key-sequence to [`LeafAction`] bindings, defaults with overrides layered
/// on top (config over defaults, spec §9).
#[derive(Debug, Clone)]
pub struct Keymap {
    bindings: HashMap<Vec<Key>, LeafAction>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

impl Keymap {
    /// The built-in bindings, spec §11.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bindings: default_bindings().into_iter().collect(),
        }
    }

    /// Layer `overrides` over `self`; a key sequence already bound is
    /// replaced, matching "config over defaults".
    #[must_use]
    pub fn with_overrides(
        mut self,
        overrides: impl IntoIterator<Item = (Vec<Key>, LeafAction)>,
    ) -> Self {
        for (k, v) in overrides {
            self.bindings.insert(k, v);
        }
        self
    }

    pub fn bind(&mut self, keys: Vec<Key>, action: LeafAction) {
        self.bindings.insert(keys, action);
    }

    #[must_use]
    pub fn lookup(&self, buf: &[Key]) -> Lookup {
        let exact = self.bindings.get(buf).cloned();
        let is_prefix_of_longer = self
            .bindings
            .keys()
            .any(|k| k.len() > buf.len() && k.starts_with(buf));
        match (exact, is_prefix_of_longer) {
            (Some(leaf), false) => Lookup::Match(leaf),
            (Some(leaf), true) => Lookup::PendingWithFallback(leaf),
            (None, true) => Lookup::Pending,
            (None, false) => Lookup::NoMatch,
        }
    }
}

fn k(s: &str) -> Vec<Key> {
    Key::parse_str(s)
}

fn motion(m: BuiltinMotion) -> LeafAction {
    LeafAction::Motion(m)
}

fn op(o: Operator) -> LeafAction {
    LeafAction::Operator(o)
}

fn standalone(a: Action) -> LeafAction {
    LeafAction::Standalone(a)
}

/// The default keymap, spec §11 plus the tables in §3-§6.1.
#[must_use]
pub fn default_bindings() -> Vec<(Vec<Key>, LeafAction)> {
    use Direction::{Backward, Forward};
    vec![
        // -- frame-accurate movement (§3.1) --
        (k("<Left>"), motion(BuiltinMotion::Frame(Backward))),
        (k("<Right>"), motion(BuiltinMotion::Frame(Forward))),
        // -- jump points (§3.2) --
        (k("h"), motion(BuiltinMotion::JumpPoint(Backward))),
        (k("l"), motion(BuiltinMotion::JumpPoint(Forward))),
        // -- track focus (§3.1) --
        (k("j"), motion(BuiltinMotion::TrackStep(Forward))),
        (k("k"), motion(BuiltinMotion::TrackStep(Backward))),
        (k("]t"), motion(BuiltinMotion::TrackCycle(Forward))),
        (k("[t"), motion(BuiltinMotion::TrackCycle(Backward))),
        // -- clip/edit-point motions (§3.3) --
        (k("w"), motion(BuiltinMotion::ClipBoundary(Forward))),
        (k("b"), motion(BuiltinMotion::ClipBoundary(Backward))),
        (k("e"), motion(BuiltinMotion::ClipEnd)),
        (k("0"), motion(BuiltinMotion::TimelineStart)),
        (k("$"), motion(BuiltinMotion::TimelineEnd)),
        (k("gg"), motion(BuiltinMotion::TimelineStart)),
        (k("G"), motion(BuiltinMotion::TimelineEnd)),
        (k("{"), motion(BuiltinMotion::Marker(Backward))),
        (k("}"), motion(BuiltinMotion::Marker(Forward))),
        (k("%"), motion(BuiltinMotion::MatchingEdit)),
        // -- editing verbs (§4) --
        (k("s"), standalone(Action::SplitCurrent)),
        (k("gs"), standalone(Action::SplitAll)),
        (k("x"), standalone(Action::RippleDeleteClip)),
        (k("d"), op(Operator::RippleDelete)),
        (k("gd"), op(Operator::Lift)),
        (k("y"), op(Operator::Yank)),
        (k("c"), op(Operator::Change)),
        (
            k("p"),
            standalone(Action::Paste {
                before: false,
                ripple: true,
                register: None,
            }),
        ),
        (
            k("P"),
            standalone(Action::Paste {
                before: true,
                ripple: true,
                register: None,
            }),
        ),
        (
            k("gp"),
            standalone(Action::Paste {
                before: false,
                ripple: false,
                register: None,
            }),
        ),
        (
            k("gP"),
            standalone(Action::Paste {
                before: true,
                ripple: false,
                register: None,
            }),
        ),
        (k("r"), standalone(Action::Replace)),
        (k("i"), standalone(Action::InsertMedia)),
        (k("a"), standalone(Action::AppendMedia)),
        (k("u"), standalone(Action::Undo)),
        (k("<C-r>"), standalone(Action::Redo)),
        (k("."), standalone(Action::Repeat)),
        (k("q"), LeafAction::NeedsArg(ArgKind::MacroStart)),
        (k("@"), LeafAction::NeedsArg(ArgKind::MacroReplay)),
        // -- trim family (§4.0.1) --
        (k("t"), op(Operator::RippleTrim)),
        (k("gt"), op(Operator::Roll)),
        (k("T"), op(Operator::Slip)),
        (k("gT"), op(Operator::Slide)),
        (
            k("<"),
            standalone(Action::TrimEdgeStep {
                forward: false,
                count: 1,
            }),
        ),
        (
            k(">"),
            standalone(Action::TrimEdgeStep {
                forward: true,
                count: 1,
            }),
        ),
        // -- visual mode (§6) --
        (k("v"), standalone(Action::EnterVisual(Mode::Visual))),
        (k("V"), standalone(Action::EnterVisual(Mode::VisualLine))),
        (
            k("<C-v>"),
            standalone(Action::EnterVisual(Mode::VisualBlock)),
        ),
        (k("o"), standalone(Action::SwapVisualEnds)),
        // -- marks (§3.3) --
        (k("m"), LeafAction::NeedsArg(ArgKind::SetMark)),
        (k("`"), LeafAction::NeedsArg(ArgKind::JumpMark)),
        // -- audio (§6.1) --
        (k("f"), op(Operator::Fade)),
        (k("+"), standalone(Action::GainAdjust(1))),
        (k("-"), standalone(Action::GainAdjust(-1))),
        // -- transitions (§6.2) --
        (k("gx"), standalone(Action::CreateTransition)),
        (k("dax"), standalone(Action::DeleteTransition)),
        // -- transport (§3.2.1) --
        (k("<Space><Space>"), standalone(Action::PlayPause)),
        (k("J"), standalone(Action::Shuttle { forward: false })),
        (k("L"), standalone(Action::Shuttle { forward: true })),
        (k("K"), standalone(Action::ShuttleStop)),
        (k("<Space>p"), standalone(Action::PreviewAndReturn)),
        (k("<Space>l"), standalone(Action::LoopSelection)),
        // -- command mode / escape --
        (k(":"), standalone(Action::EnterCommandMode)),
        (k("<Esc>"), standalone(Action::Escape)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_prefix_of_nothing_longer_matches_immediately() {
        let km = Keymap::new();
        assert_eq!(
            km.lookup(&k("s")),
            Lookup::Match(standalone(Action::SplitCurrent))
        );
    }

    #[test]
    fn an_ambiguous_prefix_is_pending() {
        let km = Keymap::new();
        // "g" alone is not bound, but is a prefix of "gs", "gd", "gg"...
        assert_eq!(km.lookup(&k("g")), Lookup::Pending);
    }

    #[test]
    fn a_bound_key_that_also_prefixes_a_longer_one_carries_a_fallback() {
        let km = Keymap::new();
        // "d" is bound (RippleDelete) and also prefixes "dax".
        assert_eq!(
            km.lookup(&k("d")),
            Lookup::PendingWithFallback(op(Operator::RippleDelete))
        );
    }

    #[test]
    fn user_overrides_win_over_defaults_and_can_disambiguate() {
        // A user maps `gx` to something else while `gd` still exists;
        // resolution must still see `gd` and the new `gx` correctly.
        let km = Keymap::new().with_overrides([(k("gx"), standalone(Action::Undo))]);
        assert_eq!(km.lookup(&k("gx")), Lookup::Match(standalone(Action::Undo)));
        assert_eq!(km.lookup(&k("gd")), Lookup::Match(op(Operator::Lift)));
    }

    #[test]
    fn unbound_and_unprefixed_keys_do_not_match() {
        let km = Keymap::new();
        assert_eq!(km.lookup(&k("Z")), Lookup::NoMatch);
    }
}
