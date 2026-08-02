//! The grammar's output: what a fully-parsed key sequence means (spec §3,
//! §4, §4.0.1, §6, §11).
//!
//! [`Action`] is deliberately inert - it names an intent, nothing more, so
//! [`crate::parser::Parser`] stays free of `Timeline`/`Session` access and
//! [`crate::engine::Engine`] is the only place grammar becomes a mutation
//! (see the crate-level docs).

use crate::mode::Mode;
use vimci_motion::{BuiltinMotion, TextObject};

/// A verb that takes a target (spec §4, §4.0.1, §6.1).
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
    /// The verb ran with a motion after it (`dw`, `t\``a\```, ...); the
    /// range/delta is `[playhead, motion target)`.
    Motion(BuiltinMotion, u32),
    /// A text object (`ic`/`ac`/`it`/`at`/`is`).
    Object(TextObject),
    /// The doubled form (`dd`/`yy`/`cc`): the whole clip under the playhead.
    WholeClip,
    /// Applied while in a VISUAL mode: use the live selection.
    Visual,
}

/// What a single non-composing keystroke needs typed after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    SetMark,
    JumpMark,
    MacroStart,
    MacroReplay,
}

/// A fully parsed key sequence, mode-agnostic. [`crate::engine::Engine`]
/// gives it meaning against the current [`Mode`] and [`vimci_cmd::Session`].
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
    /// Toggle a track in/out of a `VISUAL-BLOCK` selection.
    ToggleVisualTrack,
    /// `<` / `>`: trim the nearest edge by one jump point.
    TrimEdgeStep {
        forward: bool,
        count: u32,
    },
    /// `+` / `-`: adjust gain in dB.
    GainAdjust(i32),
    /// `gx`: create a transition at the nearest cut.
    CreateTransition,
    /// `dax`: delete the transition at the nearest cut.
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
    /// A host-owned callback, bound by a Lua `map(mode, lhs, function)`
    /// (spec §9.2). The id is opaque here on purpose: `vimci-keys` must not
    /// depend on `vimci-lua`, so the engine reports it back and the host
    /// invokes it.
    Plugin(u32),
    /// `:`.
    EnterCommandMode,
    /// `Esc`.
    Escape,
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
