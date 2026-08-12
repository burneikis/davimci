//! The input grammar: `[count] [register] operator [count]
//! motion|textobject`, plus standalone commands, `g`-prefixed sequences, and
//! `<Space>` leader sequences.
//!
//! [`Parser`] is a pure state machine: feed it [`Key`]s, get back a [`Step`].
//! It never touches a [`davimci_core::Timeline`] - that is
//! [`crate::engine::Engine`]'s job, once a [`Action`] exists to act on. This
//! is what makes `"3dw"`-style golden tests possible without a fixture
//! timeline.

use crate::action::{Action, ArgKind, LeafAction, Operator, Target};
use crate::key::Key;
use crate::keymap::{Keymap, Lookup};
use crate::mode::Mode;

/// What one [`Parser::feed`] call produced.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// More keys are needed; an ambiguous prefix should wait for the
    /// keymap's configured timeout before falling back (ties to
    /// `PendingWithFallback`, resolved by [`Parser::timeout`]).
    Pending,
    /// A full action resolved. The parser has reset to idle.
    Complete(Action),
    /// `Esc` cancelled whatever was pending. The parser has reset to idle.
    Cancelled,
    /// The buffered keys do not and cannot form a valid sequence. The parser
    /// has reset to idle.
    Invalid,
}

#[derive(Debug, Clone, PartialEq)]
enum St {
    Idle,
    Count1(u32),
    AwaitRegisterChar {
        count1: Option<u32>,
    },
    HaveRegister {
        count1: Option<u32>,
        register: char,
    },
    Buffering {
        count1: Option<u32>,
        register: Option<char>,
        buf: Vec<Key>,
        fallback: Option<(usize, LeafAction)>,
    },
    NeedsArg {
        count1: Option<u32>,
        register: Option<char>,
        arg: ArgKind,
    },
    OperatorTarget {
        op: Operator,
        trigger: Vec<Key>,
        count1: Option<u32>,
        register: Option<char>,
        count2: Option<u32>,
        buf: Vec<Key>,
    },
    /// `i`/`a` typed in a VISUAL mode, waiting for the object's name.
    VisualObject {
        around: bool,
    },
    OperatorObject {
        op: Operator,
        count1: Option<u32>,
        register: Option<char>,
        count2: Option<u32>,
        wide: bool,
    },
}

/// The key-sequence parser. Mode-aware only for the one place the grammar
/// genuinely branches on it: an operator in a `VISUAL*` mode applies to the
/// live selection immediately rather than waiting for a motion.
#[derive(Debug, Clone)]
pub struct Parser {
    state: St,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    #[must_use]
    pub fn new() -> Self {
        Self { state: St::Idle }
    }

    /// True while a sequence is mid-flight - the caller's ambiguity timeout
    /// only matters in this state.
    #[must_use]
    pub fn is_pending(&self) -> bool {
        !matches!(self.state, St::Idle)
    }

    /// The keys buffered so far, as the table would match them.
    ///
    /// Only the part a [`crate::Keymap`] lookup takes: a count or a register
    /// prefix is grammar, not a table entry, so it is reported separately by
    /// [`Self::pending_text`].
    #[must_use]
    pub fn pending_keys(&self) -> &[Key] {
        match &self.state {
            St::Buffering { buf, .. } | St::OperatorTarget { buf, .. } => buf,
            _ => &[],
        }
    }

    /// The half-typed sequence as the user typed it, count and register
    /// included - what a status line or a which-key panel shows.
    #[must_use]
    pub fn pending_text(&self) -> String {
        let prefix = |count1: Option<u32>, register: Option<char>| {
            let mut s = String::new();
            if let Some(n) = count1 {
                s.push_str(&n.to_string());
            }
            if let Some(r) = register {
                s.push('"');
                s.push(r);
            }
            s
        };
        match &self.state {
            St::Idle => String::new(),
            St::Count1(n) => n.to_string(),
            St::AwaitRegisterChar { count1 } => format!("{}\"", prefix(*count1, None)),
            St::HaveRegister { count1, register } => prefix(*count1, Some(*register)),
            St::Buffering {
                count1,
                register,
                buf,
                ..
            } => format!("{}{}", prefix(*count1, *register), crate::docs::render(buf)),
            St::NeedsArg {
                count1, register, ..
            } => prefix(*count1, *register),
            St::OperatorTarget {
                op,
                trigger,
                count1,
                register,
                count2,
                buf,
            } => {
                let _ = op;
                let mut s = prefix(*count1, *register);
                s.push_str(&crate::docs::render(trigger));
                if let Some(n) = count2 {
                    s.push_str(&n.to_string());
                }
                s.push_str(&crate::docs::render(buf));
                s
            }
            St::VisualObject { around } => {
                if *around {
                    "a".to_string()
                } else {
                    "i".to_string()
                }
            }
            St::OperatorObject {
                op,
                count1,
                register,
                count2,
                wide,
            } => {
                let _ = op;
                let mut s = prefix(*count1, *register);
                if let Some(n) = count2 {
                    s.push_str(&n.to_string());
                }
                s.push(if *wide { 'a' } else { 'i' });
                s
            }
        }
    }

    /// Drop any half-typed sequence. Used on `Esc` and when the editor
    /// switches to a different timeline, where a pending count or operator
    /// would otherwise apply to the wrong one.
    pub fn reset(&mut self) {
        self.state = St::Idle;
    }

    /// Resolve whatever `PendingWithFallback` is currently buffered, as a
    /// caller does on an ambiguity timeout for an ambiguous prefix.
    pub fn timeout(&mut self) -> Step {
        match std::mem::replace(&mut self.state, St::Idle) {
            St::Buffering {
                count1,
                register,
                fallback: Some((_, leaf)),
                ..
            } => self.resolve_leaf(leaf, count1, register, Mode::Normal),
            other => {
                self.state = other;
                Step::Pending
            }
        }
    }

    pub fn feed(&mut self, key: Key, keymap: &Keymap, mode: Mode) -> Step {
        if key.is_esc() {
            self.reset();
            return Step::Cancelled;
        }
        match std::mem::replace(&mut self.state, St::Idle) {
            St::Idle => self.at_start(key, keymap, mode, None),
            St::Count1(n) => self.after_count1(key, keymap, mode, n),
            St::AwaitRegisterChar { count1 } => self.await_register_char(key, count1),
            St::HaveRegister { count1, register } => {
                self.at_start(key, keymap, mode, Some((count1, register)))
            }
            St::Buffering {
                count1,
                register,
                mut buf,
                fallback,
            } => {
                buf.push(key);
                self.continue_buffering(keymap, mode, count1, register, buf, fallback)
            }
            St::NeedsArg {
                count1,
                register,
                arg,
            } => self.needs_arg(key, count1, register, arg),
            St::OperatorTarget {
                op,
                trigger,
                count1,
                register,
                count2,
                buf,
            } => self.operator_target(
                key, keymap, mode, op, trigger, count1, register, count2, buf,
            ),
            St::OperatorObject {
                op,
                count1,
                register,
                count2,
                wide,
            } => self.operator_object(key, keymap, op, count1, register, count2, wide),
            St::VisualObject { around } => self.visual_object(key, around),
        }
    }

    /// The object typed after `i`/`a` while a selection is live.
    /// Only the track objects mean anything here: they narrow the scope of
    /// what is already selected rather than resolving a new range.
    fn visual_object(&mut self, key: Key, around: bool) -> Step {
        self.reset();
        match key {
            Key::Char('t') => Step::Complete(Action::NarrowSelection { group: around }),
            _ => Step::Invalid,
        }
    }

    // -- start of a sequence: optional count, optional register ----------

    fn at_start(
        &mut self,
        key: Key,
        keymap: &Keymap,
        mode: Mode,
        pre: Option<(Option<u32>, char)>,
    ) -> Step {
        let (count1, register) = pre.map_or((None, None), |(c, r)| (c, Some(r)));
        // In a VISUAL mode `i`/`a` start a text object, not a media insert:
        // typing `it` narrows the live selection to a track.
        if mode.is_visual()
            && let Some(around) = match key {
                Key::Char('i') => Some(false),
                Key::Char('a') => Some(true),
                _ => None,
            }
        {
            self.state = St::VisualObject { around };
            return Step::Pending;
        }
        if register.is_none() {
            if let Some(d) = digit(key)
                && (d != 0 || count1.is_some())
            {
                let n = push_digit(count1.unwrap_or(0), d);
                self.state = St::Count1(n);
                return Step::Pending;
            }
            if key == Key::Char('"') {
                self.state = St::AwaitRegisterChar { count1 };
                return Step::Pending;
            }
        }
        self.continue_buffering(keymap, mode, count1, register, vec![key], None)
    }

    fn after_count1(&mut self, key: Key, keymap: &Keymap, mode: Mode, n: u32) -> Step {
        if let Some(d) = digit(key) {
            self.state = St::Count1(push_digit(n, d));
            return Step::Pending;
        }
        if key == Key::Char('"') {
            self.state = St::AwaitRegisterChar { count1: Some(n) };
            return Step::Pending;
        }
        self.continue_buffering(keymap, mode, Some(n), None, vec![key], None)
    }

    fn await_register_char(&mut self, key: Key, count1: Option<u32>) -> Step {
        match key.as_char() {
            Some(c) if c.is_alphanumeric() => {
                self.state = St::HaveRegister {
                    count1,
                    register: c,
                };
                Step::Pending
            }
            _ => {
                self.reset();
                Step::Invalid
            }
        }
    }

    // -- literal-sequence resolution (keymap) -----------------------------

    fn continue_buffering(
        &mut self,
        keymap: &Keymap,
        mode: Mode,
        count1: Option<u32>,
        register: Option<char>,
        buf: Vec<Key>,
        prior_fallback: Option<(usize, LeafAction)>,
    ) -> Step {
        match keymap.lookup(&buf) {
            Lookup::Match(leaf) => self.resolve_leaf(leaf, count1, register, mode),
            Lookup::Pending => {
                self.state = St::Buffering {
                    count1,
                    register,
                    buf,
                    fallback: prior_fallback,
                };
                Step::Pending
            }
            // In a VISUAL mode an operator always acts on the live
            // selection at once: it can never be the start of a
            // longer literal sequence like `dax`, so the ambiguity the
            // default table has in NORMAL mode does not apply here.
            Lookup::PendingWithFallback(leaf @ LeafAction::Operator(_)) if mode.is_visual() => {
                self.resolve_leaf(leaf, count1, register, mode)
            }
            Lookup::PendingWithFallback(leaf) => {
                let len = buf.len();
                self.state = St::Buffering {
                    count1,
                    register,
                    buf,
                    fallback: Some((len, leaf)),
                };
                Step::Pending
            }
            Lookup::NoMatch => {
                if let Some((len, leaf)) = prior_fallback {
                    let remaining = buf[len..].to_vec();
                    let resolved = self.resolve_leaf(leaf, count1, register, mode);
                    // The fallback can only usefully be an operator (the
                    // only ambiguity the default table produces, e.g. `d`
                    // vs `dax`): replay the keys typed after it as its
                    // target. Any other leaf kind with leftover keys is a
                    // configuration mistake, not a valid sequence.
                    if let Step::Pending = resolved {
                        for k in remaining {
                            match self.feed(k, keymap, mode) {
                                Step::Pending => {}
                                other => return other,
                            }
                        }
                        return Step::Pending;
                    }
                    self.reset();
                    return Step::Invalid;
                }
                self.reset();
                Step::Invalid
            }
        }
    }

    fn resolve_leaf(
        &mut self,
        leaf: LeafAction,
        count1: Option<u32>,
        register: Option<char>,
        mode: Mode,
    ) -> Step {
        match leaf {
            LeafAction::Motion(m) => {
                self.reset();
                Step::Complete(Action::Move {
                    motion: m,
                    count: count1.unwrap_or(1),
                })
            }
            LeafAction::Standalone(action) => {
                self.reset();
                Step::Complete(instantiate(action, count1, register))
            }
            LeafAction::NeedsArg(arg) => {
                self.state = St::NeedsArg {
                    count1,
                    register,
                    arg,
                };
                Step::Pending
            }
            LeafAction::Operator(op) => {
                if mode.is_visual() {
                    self.reset();
                    return Step::Complete(Action::Verb {
                        op,
                        count: count1.unwrap_or(1),
                        register,
                        target: Target::Visual,
                    });
                }
                self.state = St::OperatorTarget {
                    op,
                    trigger: op_trigger(op),
                    count1,
                    register,
                    count2: None,
                    buf: Vec::new(),
                };
                Step::Pending
            }
        }
    }

    fn needs_arg(
        &mut self,
        key: Key,
        count1: Option<u32>,
        register: Option<char>,
        arg: ArgKind,
    ) -> Step {
        self.reset();
        let Some(c) = key.as_char() else {
            return Step::Invalid;
        };
        let action = match arg {
            ArgKind::SetMark => Action::SetMark(c),
            ArgKind::JumpMark => Action::JumpMark(c),
            ArgKind::MacroStart => Action::MacroStart(c),
            ArgKind::MacroReplay => Action::MacroReplay(c, count1.unwrap_or(1)),
        };
        let _ = register;
        Step::Complete(action)
    }

    // -- operator target: motion, text object, or doubled trigger --------

    #[allow(clippy::too_many_arguments)]
    fn operator_target(
        &mut self,
        key: Key,
        keymap: &Keymap,
        mode: Mode,
        op: Operator,
        trigger: Vec<Key>,
        count1: Option<u32>,
        register: Option<char>,
        count2: Option<u32>,
        mut buf: Vec<Key>,
    ) -> Step {
        let _ = mode;
        if buf.is_empty()
            && let Some(d) = digit(key)
            && (d != 0 || count2.is_some())
        {
            let n = push_digit(count2.unwrap_or(0), d);
            self.state = St::OperatorTarget {
                op,
                trigger,
                count1,
                register,
                count2: Some(n),
                buf,
            };
            return Step::Pending;
        }
        buf.push(key);
        if buf == trigger {
            self.reset();
            return Step::Complete(Action::Verb {
                op,
                count: merge_count(count1, count2),
                register,
                target: Target::WholeClip,
            });
        }
        if buf.len() == 1 {
            if let Key::Char('i') = key {
                self.state = St::OperatorObject {
                    op,
                    count1,
                    register,
                    count2,
                    wide: false,
                };
                return Step::Pending;
            }
            if let Key::Char('a') = key {
                self.state = St::OperatorObject {
                    op,
                    count1,
                    register,
                    count2,
                    wide: true,
                };
                return Step::Pending;
            }
        }
        match keymap.lookup(&buf) {
            Lookup::Match(LeafAction::Motion(m)) => {
                self.reset();
                Step::Complete(Action::Verb {
                    op,
                    count: merge_count(count1, count2),
                    register,
                    target: Target::Motion(m, merge_count(count1, count2)),
                })
            }
            Lookup::Pending | Lookup::PendingWithFallback(_) => {
                self.state = St::OperatorTarget {
                    op,
                    trigger,
                    count1,
                    register,
                    count2,
                    buf,
                };
                Step::Pending
            }
            _ => {
                self.reset();
                Step::Invalid
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn operator_object(
        &mut self,
        key: Key,
        keymap: &Keymap,
        op: Operator,
        count1: Option<u32>,
        register: Option<char>,
        count2: Option<u32>,
        wide: bool,
    ) -> Step {
        self.reset();
        // A config-registered name is looked up first, so config
        // wins over defaults here exactly as it does for keymaps.
        if let Key::Char(c) = key
            && keymap.has_object(c)
        {
            return Step::Complete(Action::Verb {
                op,
                count: merge_count(count1, count2),
                register,
                target: Target::Object(davimci_motion::TextObject::Named {
                    name: c,
                    around: wide,
                }),
            });
        }
        let object = match (key, wide) {
            (Key::Char('c'), false) => davimci_motion::TextObject::InnerClip,
            (Key::Char('c'), true) => davimci_motion::TextObject::AClip,
            (Key::Char('t'), false) => davimci_motion::TextObject::InnerTrack,
            (Key::Char('t'), true) => davimci_motion::TextObject::ATrack,
            (Key::Char('s'), false) => davimci_motion::TextObject::InnerSegment(None),
            _ => return Step::Invalid,
        };
        Step::Complete(Action::Verb {
            op,
            count: merge_count(count1, count2),
            register,
            target: Target::Object(object),
        })
    }
}

/// Largest accepted count. Vim clamps rather than rejecting a
/// long digit run, and so do we: no count a user can type may overflow.
pub const MAX_COUNT: u32 = 1_000_000;

/// Append a digit to a count, clamping at [`MAX_COUNT`].
fn push_digit(n: u32, d: u32) -> u32 {
    n.saturating_mul(10).saturating_add(d).min(MAX_COUNT)
}

fn merge_count(a: Option<u32>, b: Option<u32>) -> u32 {
    a.unwrap_or(1)
        .saturating_mul(b.unwrap_or(1))
        .clamp(1, MAX_COUNT)
}

fn digit(key: Key) -> Option<u32> {
    match key {
        Key::Char(c) if c.is_ascii_digit() => c.to_digit(10),
        _ => None,
    }
}

fn op_trigger(op: Operator) -> Vec<Key> {
    let s = match op {
        Operator::RippleDelete => "d",
        Operator::Lift => "gd",
        Operator::Yank => "y",
        Operator::Change => "c",
        Operator::RippleTrim => "t",
        Operator::Roll => "gt",
        Operator::Slip => "T",
        Operator::Slide => "gT",
        Operator::Fade => "f",
    };
    Key::parse_str(s)
}

/// Fill in the count/register a standalone leaf's template did not carry
/// (the keymap's `Standalone` entries are written with placeholder defaults
/// since counts/registers are grammar, not table data).
fn instantiate(action: Action, count1: Option<u32>, register: Option<char>) -> Action {
    match action {
        Action::Paste { before, ripple, .. } => Action::Paste {
            before,
            ripple,
            register,
        },
        Action::TrimEdgeStep { forward, .. } => Action::TrimEdgeStep {
            forward,
            count: count1.unwrap_or(1),
        },
        Action::ShiftClips { forward, .. } => Action::ShiftClips {
            forward,
            count: count1.unwrap_or(1),
        },
        Action::GainAdjust(step) => {
            Action::GainAdjust(step.saturating_mul(count_as_i32(count1.unwrap_or(1))))
        }
        other => other,
    }
}

/// A count is a repeat, so a count too large to express saturates rather than
/// wrapping into a negative step.
fn count_as_i32(count: u32) -> i32 {
    i32::try_from(count).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Operator as Op, Target};
    use davimci_motion::{BuiltinMotion, Direction, TextObject};

    fn run(s: &str) -> Step {
        let keymap = Keymap::new();
        let mut p = Parser::new();
        let mut last = Step::Invalid;
        for key in Key::parse_str(s) {
            last = p.feed(key, &keymap, Mode::Normal);
        }
        last
    }

    /// Regression: `n * 10 + d` overflowed `u32` and panicked in a debug
    /// build on a long digit run. Counts clamp, exactly as vim does.
    #[test]
    fn an_absurdly_long_count_clamps_instead_of_overflowing() {
        assert_eq!(
            run("99999999999l"),
            Step::Complete(Action::Move {
                motion: BuiltinMotion::JumpPoint(Direction::Forward),
                count: MAX_COUNT,
            })
        );
        // The operator/object count multiplies too, and clamps the same way.
        assert_eq!(
            run("99999999999d99999999999w"),
            Step::Complete(Action::Verb {
                op: Op::RippleDelete,
                count: MAX_COUNT,
                register: None,
                target: Target::Motion(BuiltinMotion::ClipBoundary(Direction::Forward), MAX_COUNT),
            })
        );
    }

    #[test]
    fn three_d_w_is_a_counted_ripple_delete_to_the_next_boundary() {
        let step = run("3dw");
        assert_eq!(
            step,
            Step::Complete(Action::Verb {
                op: Op::RippleDelete,
                count: 3,
                register: None,
                target: Target::Motion(BuiltinMotion::ClipBoundary(Direction::Forward), 3),
            })
        );
    }

    #[test]
    fn d_2_i_c_multiplies_the_count_into_an_object_target() {
        let step = run("d2ic");
        assert_eq!(
            step,
            Step::Complete(Action::Verb {
                op: Op::RippleDelete,
                count: 2,
                register: None,
                target: Target::Object(TextObject::InnerClip),
            })
        );
    }

    #[test]
    fn gs_is_a_standalone_global_split() {
        assert_eq!(run("gs"), Step::Complete(Action::SplitAll));
    }

    #[test]
    fn a_named_register_prefixes_an_operator() {
        let step = run("\"ayy");
        assert_eq!(
            step,
            Step::Complete(Action::Verb {
                op: Op::Yank,
                count: 1,
                register: Some('a'),
                target: Target::WholeClip,
            })
        );
    }

    #[test]
    fn macro_record_and_replay_tokens() {
        assert_eq!(run("qa"), Step::Complete(Action::MacroStart('a')));
        assert_eq!(run("@a"), Step::Complete(Action::MacroReplay('a', 1)));
        assert_eq!(run("3@a"), Step::Complete(Action::MacroReplay('a', 3)));
    }

    #[test]
    fn a_literal_binding_over_an_operator_key_is_distinct_from_it() {
        // No transition type is core, so `dax` is a plugin's binding; what
        // the grammar owes it is that binding a literal over `d` works.
        assert_eq!(run("dax"), Step::Invalid);
        // `d` alone followed by a genuine motion still works.
        assert_eq!(
            run("dw"),
            Step::Complete(Action::Verb {
                op: Op::RippleDelete,
                count: 1,
                register: None,
                target: Target::Motion(BuiltinMotion::ClipBoundary(Direction::Forward), 1),
            })
        );
    }

    #[test]
    fn doubling_an_operator_targets_the_whole_clip() {
        for (s, op) in [
            ("dd", Op::RippleDelete),
            ("yy", Op::Yank),
            ("cc", Op::Change),
        ] {
            assert_eq!(
                run(s),
                Step::Complete(Action::Verb {
                    op,
                    count: 1,
                    register: None,
                    target: Target::WholeClip,
                }),
                "{s} did not resolve to a whole-clip target"
            );
        }
    }

    #[test]
    fn esc_cancels_from_any_pending_state() {
        let keymap = Keymap::new();
        for prefix in ["3", "\"", "d", "g", "\"ad2"] {
            let mut p = Parser::new();
            for key in Key::parse_str(prefix) {
                p.feed(key, &keymap, Mode::Normal);
            }
            assert!(p.is_pending(), "{prefix} should be mid-sequence");
            assert_eq!(
                p.feed(Key::parse_str("<Esc>")[0], &keymap, Mode::Normal),
                Step::Cancelled
            );
            assert!(!p.is_pending());
        }
    }

    #[test]
    fn an_operator_in_visual_mode_applies_to_the_selection_directly() {
        let keymap = Keymap::new();
        let mut p = Parser::new();
        let step = p.feed(Key::Char('d'), &keymap, Mode::Visual);
        assert_eq!(
            step,
            Step::Complete(Action::Verb {
                op: Op::RippleDelete,
                count: 1,
                register: None,
                target: Target::Visual,
            })
        );
    }

    #[test]
    fn an_unbound_sequence_is_invalid() {
        assert_eq!(run("Z"), Step::Invalid);
    }
}
