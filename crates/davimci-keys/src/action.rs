//! The grammar's output: what a fully-parsed key sequence means.
//!
//! [`Action`] is deliberately inert - it names an intent, nothing more, so
//! [`crate::parser::Parser`] stays free of `Timeline`/`Session` access and
//! [`crate::engine::Engine`] is the only place grammar becomes a mutation
//! (see the crate-level docs).

use crate::mode::Mode;
use davimci_motion::{BuiltinMotion, TextObject};

/// A verb that takes a target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `d` / `dw` / `dd`: ripple delete.
    RippleDelete,
    /// `gd`: lift (delete, leave gap).
    Lift,
    /// `y`: yank.
    Yank,
    /// `c`: change - delete, then drop into `INSERT`.
    Change,
    /// `t` + motion: ripple trim the nearest edge to the target.
    RippleTrim,
    /// `gt` + motion: roll the cut nearest the playhead.
    Roll,
    /// `T` + motion: slip the clip under the playhead.
    Slip,
    /// `gT` + motion: slide the clip under the playhead.
    Slide,
    /// `f` + motion: fade across the motion range.
    Fade,
}

impl Operator {
    /// Whether this operator can be satisfied by `is` (a VISUAL segment),
    /// used only to keep `it`/`at`/`is` resolution honest; every operator
    /// accepts every object today, so this is a hook for future limits.
    #[must_use]
    pub fn accepts(self, _object: TextObject) -> bool {
        true
    }
}

/// What an operator acts on, once the grammar has resolved it.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// The verb ran with a motion after it (`dw`, <code>t\`</code>, <code>a\`</code>, ...); the
    /// range/delta is `[playhead, motion target)`.
    Motion(BuiltinMotion, u32),
    /// A text object (`ic`/`ac`/`it`/`at`/`is`).
    Object(TextObject),
    /// The doubled form (`dd`/`yy`/`cc`): the whole clip under the playhead.
    WholeClip,
    /// Applied while in a VISUAL mode: use the live selection.
    Visual,
    /// An already-resolved range on the focused track. The host builds this
    /// after answering a config-registered text object; nothing
    /// in the grammar produces it.
    Range(davimci_motion::TimeRange),
}

impl Action {
    /// Re-target a verb at a range the host resolved. Anything
    /// else is returned unchanged.
    #[must_use]
    pub fn with_range(self, range: davimci_motion::TimeRange) -> Self {
        match self {
            Self::Verb {
                op,
                count,
                register,
                ..
            } => Self::Verb {
                op,
                count,
                register,
                target: Target::Range(range),
            },
            other => other,
        }
    }
}

/// What a single non-composing keystroke needs typed after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    SetMark,
    JumpMark,
    MacroStart,
    MacroReplay,
}

/// A zoom step. Zoom is view state, not an edit, so this
/// never reaches the undo log; the engine hands it back to the host, which
/// owns the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoomIntent {
    /// `zi`: one level in.
    In,
    /// `zo`: one level out.
    Out,
    /// `z0`: back to the default level.
    Reset,
}

/// A request to centre the view on the playhead. The playhead does not move;
/// only the scroll position does, so this never reaches the undo log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CenterIntent {
    /// `zz`: centre once, leaving scroll-follow as it was.
    Once,
    /// `zZ`: turn permanent centring on or off.
    Toggle,
}

/// What an action does to a running preview.
///
/// Playback owns the playhead while it runs, so an action that reads or
/// writes the playhead cannot share the clock with it: the pacer would
/// overwrite a motion on the next tick and an edit would re-project a graph
/// out from under a live consumer. Rather than hard-coding "seek keys pause",
/// every action declares which it is, so a remapped or Lua-defined binding
/// gets the same treatment as a built-in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportPolicy {
    /// Stop the clock, commit the playhead where playback reached, then run.
    Interrupt,
    /// Run without touching the clock.
    Keep,
}

impl TransportPolicy {
    #[must_use]
    pub fn interrupts(self) -> bool {
        matches!(self, Self::Interrupt)
    }

    /// The stronger of two policies, for a compound action (a macro replay is
    /// as interrupting as the most interrupting key in it).
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        if self.interrupts() || other.interrupts() {
            Self::Interrupt
        } else {
            Self::Keep
        }
    }
}

/// A fully parsed key sequence, mode-agnostic. [`crate::engine::Engine`]
/// gives it meaning against the current [`Mode`] and [`davimci_cmd::Session`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A bare motion: move the playhead (`NORMAL`) or extend the selection
    /// (a `VISUAL*` mode).
    Move {
        motion: BuiltinMotion,
        count: u32,
    },
    /// An operator plus its resolved target.
    Verb {
        op: Operator,
        count: u32,
        register: Option<char>,
        target: Target,
    },
    /// `s`: split at playhead, current track (or selection in VISUAL).
    SplitCurrent,
    /// `gs`: split at playhead, all tracks.
    SplitAll,
    /// `x`: ripple delete the clip under the playhead.
    RippleDeleteClip,
    /// `p` / `P` / `gp` / `gP`.
    Paste {
        before: bool,
        ripple: bool,
        register: Option<char>,
    },
    /// `r`: replace clip under playhead (media picker is a frontend concern;
    /// this only names the intent).
    Replace,
    /// `i`: insert media at playhead.
    InsertMedia,
    /// `a`: append media after the current clip.
    AppendMedia,
    Undo,
    Redo,
    /// `.`
    Repeat,
    MacroStart(char),
    MacroStop,
    MacroReplay(char, u32),
    SetMark(char),
    JumpMark(char),
    EnterVisual(Mode),
    /// `o` inside a `VISUAL*` mode.
    SwapVisualEnds,
    /// `it` / `at` typed while a selection is live: narrow the selection to
    /// the focused track, or to its link group.
    NarrowSelection {
        group: bool,
    },
    /// `<` / `>`: trim the nearest edge by one jump point.
    TrimEdgeStep {
        forward: bool,
        count: u32,
    },
    /// `+` / `-`: adjust gain in dB.
    GainAdjust(i32),
    /// `<Space>m`: toggle mute on the current track.
    ToggleMute,
    /// `<Space>s`: toggle solo on the current track.
    ToggleSolo,
    /// Put a transition of a named type on the nearest cut. Unbound by
    /// default: no type is core, so the keys belong to whatever plugin
    /// registered the type it names.
    CreateTransition {
        kind: String,
    },
    /// Take the transition at the nearest cut away. Unbound by default, for
    /// the same reason its counterpart is.
    DeleteTransition,
    /// `<Space><Space>`.
    PlayPause,
    /// `J` / `L`.
    Shuttle {
        forward: bool,
    },
    /// `K`.
    ShuttleStop,
    /// `<Space>p`.
    PreviewAndReturn,
    /// `<Space>l`.
    LoopSelection,
    /// `zi` / `zo` / `z0`: change the zoom level.
    Zoom(ZoomIntent),
    /// `zz` / `zZ`: centre the view on the playhead.
    Center(CenterIntent),
    /// A host-owned callback, bound by a Lua `map(mode, lhs, function)`
    ///. The id is opaque here on purpose: `davimci-keys` must not
    /// depend on `davimci-lua`, so the engine reports it back and the host
    /// invokes it.
    ///
    /// `interrupt` is the binding's `{ interrupt = true }` option: a callback
    /// is `Keep` by default because the grammar cannot know whether it edits.
    Plugin {
        id: u32,
        interrupt: bool,
    },
    /// Stop playback and commit the playhead. Unbound by
    /// default; exists for user binds and `editor.interrupt_transport`.
    InterruptTransport,
    /// `:`.
    EnterCommandMode,
    /// `Esc`.
    Escape,
}

impl Action {
    /// What this action does to a running preview.
    #[must_use]
    pub fn transport_policy(&self) -> TransportPolicy {
        use TransportPolicy::{Interrupt, Keep};
        match self {
            // Everything that reads or writes the playhead, and every edit.
            Self::Move { .. }
            | Self::Verb { .. }
            | Self::SplitCurrent
            | Self::SplitAll
            | Self::RippleDeleteClip
            | Self::Paste { .. }
            | Self::Replace
            | Self::InsertMedia
            | Self::AppendMedia
            | Self::Undo
            | Self::Redo
            | Self::Repeat
            | Self::MacroReplay(..)
            | Self::JumpMark(_)
            | Self::SwapVisualEnds
            | Self::NarrowSelection { .. }
            | Self::TrimEdgeStep { .. }
            | Self::GainAdjust(_)
            | Self::ToggleMute
            | Self::ToggleSolo
            | Self::CreateTransition { .. }
            | Self::DeleteTransition
            | Self::InterruptTransport => Interrupt,
            // The transport family owns the clock itself; the rest is view
            // state, mode state, or a bookmark, none of which fight the pacer.
            Self::PlayPause
            | Self::Shuttle { .. }
            | Self::ShuttleStop
            | Self::PreviewAndReturn
            | Self::LoopSelection
            | Self::Zoom(_)
            | Self::Center(_)
            | Self::SetMark(_)
            | Self::MacroStart(_)
            | Self::MacroStop
            | Self::EnterVisual(_)
            | Self::EnterCommandMode
            | Self::Escape => Keep,
            Self::Plugin { interrupt, .. } => {
                if *interrupt {
                    Interrupt
                } else {
                    Keep
                }
            }
        }
    }
}

/// What a bound key sequence resolves to before counts/registers/targets are
/// applied - the leaves of [`crate::keymap::Keymap`].
#[derive(Debug, Clone, PartialEq)]
pub enum LeafAction {
    Motion(BuiltinMotion),
    Operator(Operator),
    /// A complete action once instantiated with whatever count/register the
    /// grammar collected ahead of it.
    Standalone(Action),
    NeedsArg(ArgKind),
}
