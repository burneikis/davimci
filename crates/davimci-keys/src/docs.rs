//! The default keymap, rendered as documentation.
//!
//! The reference table in `docs/keymap.md` is generated from
//! [`crate::keymap::default_bindings`] and checked against it by a test, so a
//! binding cannot change without the documentation changing with it. The
//! description of each action is an exhaustive match: a new [`LeafAction`]
//! stops compiling here until it has a sentence.

use crate::action::{Action, ArgKind, CenterIntent, LeafAction, Operator, ZoomIntent};
use crate::key::{Key, Named};
use crate::keymap::default_bindings;
use crate::mode::Mode;
use davimci_motion::{BuiltinMotion, Direction};

/// The whole reference document, ready to write to `docs/keymap.md`.
#[must_use]
pub fn keymap_markdown() -> String {
    use std::fmt::Write as _;

    let mut rows: Vec<(String, String, &'static str)> = default_bindings()
        .into_iter()
        .map(|(keys, action)| {
            let (section, text) = describe(&action);
            (render(&keys), text, section)
        })
        .collect();
    rows.sort_by(|a, b| {
        a.2.cmp(b.2)
            .then_with(|| sort_key(&a.0).cmp(&sort_key(&b.0)))
    });

    let mut out = String::new();
    out.push_str(
        "# Default keymap\n\n\
         Generated from the keymap table in `davimci-keys`; do not edit by hand.\n\
         Run `just docs` after changing a binding.\n\n\
         Counts, registers, marks and text objects are grammar, not bindings:\n\
         `3dw`, `\"ay`, `` `a `` and `dic` compose out of the entries below.\n\n\
         What `v` and `V` select, and how `j`/`k` widen a selection across\n\
         tracks, is in `docs/visual-mode.md`.\n",
    );
    let mut current = "";
    for (keys, text, section) in rows {
        if section != current {
            let _ = write!(out, "\n## {section}\n\n| Keys | Action |\n|---|---|\n");
            current = section;
        }
        let _ = writeln!(out, "| `{keys}` | {text} |");
    }
    out
}

/// Sort within a section by key string, but keep `<...>` names out of the way
/// of the plain characters they would otherwise interleave with.
fn sort_key(keys: &str) -> (u8, String) {
    (u8::from(keys.starts_with('<')), keys.to_lowercase())
}

/// Render a key sequence the way a config file would spell it.
#[must_use]
pub fn render(keys: &[Key]) -> String {
    keys.iter()
        .map(|k| match k {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            Key::Named(Named::Space) => "<Space>".to_string(),
            Key::Named(_) => k.to_token(),
        })
        .collect()
}

/// One sentence for what a binding does, for anything that lists bindings
/// to the user rather than to a document - a which-key panel most of all.
#[must_use]
pub fn describe_leaf(action: &LeafAction) -> String {
    describe(action).1
}

fn describe(action: &LeafAction) -> (&'static str, String) {
    match action {
        LeafAction::Motion(m) => ("Motions", motion(m)),
        LeafAction::Operator(o) => (
            "Operators",
            format!("{} (takes a motion or object)", op(*o)),
        ),
        LeafAction::NeedsArg(a) => ("Marks and macros", arg(*a).to_string()),
        LeafAction::Standalone(a) => standalone(a),
    }
}

fn motion(m: &BuiltinMotion) -> String {
    let dir = |d: Direction, fwd: &'static str, back: &'static str| match d {
        Direction::Forward => fwd,
        Direction::Backward => back,
    };
    match m {
        BuiltinMotion::Frame(d) => format!("one frame {}", dir(*d, "forward", "back")).to_string(),
        BuiltinMotion::JumpPoint(d) => {
            format!("{} jump point", dir(*d, "next", "previous"))
        }
        BuiltinMotion::TrackStep(d) => {
            format!("focus the {} track", dir(*d, "next", "previous"))
        }
        BuiltinMotion::TrackCycle(d) => {
            format!("cycle track focus {}, wrapping", dir(*d, "forward", "back"))
        }
        BuiltinMotion::ClipBoundary(d) => {
            format!("{} clip boundary", dir(*d, "next", "previous"))
        }
        BuiltinMotion::ClipEnd => "last frame of the current clip".to_string(),
        BuiltinMotion::TimelineStart => "start of the timeline".to_string(),
        BuiltinMotion::TimelineEnd => "end of the timeline".to_string(),
        BuiltinMotion::Marker(d) => format!("{} marker", dir(*d, "next", "previous")),
        BuiltinMotion::MatchingEdit => "the other end of the current clip".to_string(),
        BuiltinMotion::Mark(c) => format!("jump to mark `{c}`"),
        BuiltinMotion::Predicate(_, d) => {
            format!("{} predicate match", dir(*d, "next", "previous"))
        }
    }
}

fn op(o: Operator) -> &'static str {
    match o {
        Operator::RippleDelete => "ripple delete",
        Operator::Lift => "lift: delete and leave a gap",
        Operator::Yank => "yank",
        Operator::Change => "change: delete, then insert",
        Operator::RippleTrim => "ripple trim the nearest edge",
        Operator::Roll => "roll the nearest cut",
        Operator::Slip => "slip the clip under the playhead",
        Operator::Slide => "slide the clip under the playhead",
        Operator::Fade => "fade across the range",
    }
}

fn arg(a: ArgKind) -> &'static str {
    match a {
        ArgKind::SetMark => "set a mark, named by the next key",
        ArgKind::JumpMark => "jump to a mark, named by the next key",
        ArgKind::MacroStart => "record a macro into the register named by the next key",
        ArgKind::MacroReplay => "replay the macro in the register named by the next key",
    }
}

fn standalone(a: &Action) -> (&'static str, String) {
    let edit = "Editing";
    let visual = "Visual mode";
    let audio = "Audio";
    let transport = "Transport";
    let view = "View and modes";
    match a {
        Action::SplitCurrent => (edit, "split at the playhead on the focused track".into()),
        Action::SplitAll => (edit, "split at the playhead on every track".into()),
        Action::RippleDeleteClip => (edit, "ripple delete the clip under the playhead".into()),
        Action::Paste { before, ripple, .. } => (
            edit,
            format!(
                "paste {} the playhead, {}",
                if *before { "before" } else { "after" },
                if *ripple { "rippling" } else { "overwriting" }
            ),
        ),
        Action::Replace => (edit, "replace the clip under the playhead".into()),
        Action::InsertMedia => (edit, "insert media at the playhead".into()),
        Action::AppendMedia => (edit, "append media after the current clip".into()),
        Action::Undo => (edit, "undo".into()),
        Action::Redo => (edit, "redo".into()),
        Action::Repeat => (edit, "repeat the last edit".into()),
        Action::TrimEdgeStep { forward, .. } => (
            edit,
            format!(
                "trim the nearest edge one jump point {}",
                if *forward { "later" } else { "earlier" }
            ),
        ),
        Action::CreateTransition => (edit, "create a transition at the nearest cut".into()),
        Action::DeleteTransition => (edit, "delete the transition at the nearest cut".into()),
        Action::EnterVisual(mode) => (
            visual,
            match mode {
                Mode::VisualLine => "select whole clips (visual-line)".into(),
                _ => "select from the frame under the cursor (visual)".into(),
            },
        ),
        Action::SwapVisualEnds => (visual, "swap the ends of the selection".into()),
        Action::NarrowSelection { group } => (
            visual,
            if *group {
                "narrow the selection to the link group".into()
            } else {
                "narrow the selection to the focused track".into()
            },
        ),
        Action::GainAdjust(db) => (audio, format!("adjust gain by {db:+} dB")),
        Action::ToggleMute => (audio, "mute or unmute the focused track".into()),
        Action::ToggleSolo => (audio, "solo or unsolo the focused track".into()),
        Action::PlayPause => (transport, "play or pause".into()),
        Action::Shuttle { forward } => (
            transport,
            format!("shuttle {}", if *forward { "forward" } else { "back" }),
        ),
        Action::ShuttleStop => (transport, "stop shuttling".into()),
        Action::PreviewAndReturn => (transport, "preview, then return to the playhead".into()),
        Action::LoopSelection => (transport, "loop the selection".into()),
        Action::InterruptTransport => (transport, "stop playback and keep the playhead".into()),
        Action::Zoom(z) => (
            view,
            match z {
                ZoomIntent::In => "zoom in one level".into(),
                ZoomIntent::Out => "zoom out one level".into(),
                ZoomIntent::Reset => "reset the zoom level".into(),
            },
        ),
        Action::Center(c) => (
            view,
            match c {
                CenterIntent::Once => "centre the view on the playhead".into(),
                CenterIntent::Toggle => "keep the playhead centred".into(),
            },
        ),
        Action::EnterCommandMode => (view, "open the `:` line".into()),
        Action::Escape => (view, "leave the current mode".into()),
        // Bound only by the grammar or by config, never by the table; kept
        // exhaustive so a new action cannot slip past undocumented.
        Action::Move { .. } => ("Motions", "move the playhead".into()),
        Action::Verb { op: o, .. } => ("Operators", op(*o).to_string()),
        Action::SetMark(c) => ("Marks and macros", format!("set mark `{c}`")),
        Action::JumpMark(c) => ("Marks and macros", format!("jump to mark `{c}`")),
        Action::MacroStart(c) => ("Marks and macros", format!("record into `{c}`")),
        Action::MacroStop => ("Marks and macros", "stop recording".into()),
        Action::MacroReplay(c, _) => ("Marks and macros", format!("replay `{c}`")),
        Action::Plugin { .. } => (view, "run a config-registered callback".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_binding_reaches_the_document() {
        let doc = keymap_markdown();
        for (keys, _) in default_bindings() {
            let spelled = render(&keys);
            assert!(
                doc.contains(&format!("| `{spelled}` |")),
                "`{spelled}` is bound but undocumented"
            );
        }
    }

    #[test]
    fn the_leader_is_spelled_the_way_a_config_would_spell_it() {
        assert_eq!(render(&Key::parse_str("<Space>m")), "<Space>m");
        assert_eq!(render(&Key::parse_str("<C-r>")), "<C-r>");
    }
}
