//! What Lua asks the editor to do.
//!
//! The architectural rule is that all mutation goes through a `Command`, so
//! `davimci.editor` does not edit: it appends a [`Request`], and the host
//! (a frontend, or the headless test harness) turns each one into a
//! `davimci_keys::Action` and runs it against the session. That keeps the undo
//! log, `.`-repeat, and macros authoritative even when a plugin drives the
//! edit.

use std::collections::BTreeMap;
use std::fmt;

use davimci_keys::Action;

/// A scalar handed from Lua to a registered motion or object.
#[derive(Debug, Clone, PartialEq)]
pub enum OptValue {
    Str(String),
    Num(f64),
    Bool(bool),
}

impl OptValue {
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Self::Num(n) => Some(*n),
            _ => None,
        }
    }
}

impl fmt::Display for OptValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(s) => write!(f, "{s}"),
            Self::Num(n) => write!(f, "{n}"),
            Self::Bool(b) => write!(f, "{b}"),
        }
    }
}

/// The options table passed to `motions.run(name, opts)`, flattened to
/// scalars so a queued request stays plain data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Opts(BTreeMap<String, OptValue>);

impl Opts {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl Into<String>, value: OptValue) {
        self.0.insert(key.into(), value);
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&OptValue> {
        self.0.get(key)
    }

    #[must_use]
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(OptValue::as_str)
    }

    #[must_use]
    pub fn num(&self, key: &str) -> Option<f64> {
        self.0.get(key).and_then(OptValue::as_num)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &OptValue)> {
        self.0.iter()
    }
}

/// Something user Lua asked the editor to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Run a key-grammar action through the session, exactly as if typed.
    Edit(Action),
    /// `require("davimci.export").run(name)`.
    Export { preset: String },
    /// `require("davimci.motions").run(name, opts)` - resolved by the host,
    /// which owns the timeline and the analysis index.
    Motion { name: String, opts: Opts },
    /// `require("davimci.media").import(path)`.
    Import { path: String },
    /// `require("davimci.media").analyze(track)` - re-run analysis after a
    /// gain or fade change (plan.md Phase 9e).
    Analyze { track: Option<String> },
    /// `require("davimci.editor").message(text)`, for the status line.
    Message(String),
    /// `require("davimci.editor").set(property, value)` - `:set`, so a config
    /// can state a view setting the session would otherwise have to type.
    Set { property: String, value: String },
}

/// Map an `editor.*` string from a keymap right-hand side (spec 9.2) onto
/// the key grammar. Unknown strings are rejected at `map()` time so the user
/// hears about a typo when the config loads, not when the key is pressed.
#[must_use]
pub fn parse_editor_command(rhs: &str) -> Option<Action> {
    use davimci_motion::{BuiltinMotion, Direction};

    let rhs = rhs.trim();
    let (name, arg) = match rhs.split_once('(') {
        Some((n, rest)) => (n.trim(), Some(rest.trim_end().trim_end_matches(')').trim())),
        None => (rhs, None),
    };
    let name = name.strip_prefix("editor.").unwrap_or(name);
    let count = |arg: Option<&str>| -> Option<i64> { arg.and_then(|a| a.parse::<i64>().ok()) };

    Some(match name {
        "split_at_playhead" => Action::SplitCurrent,
        "split_all_tracks" => Action::SplitAll,
        "ripple_delete" => Action::RippleDeleteClip,
        "undo" => Action::Undo,
        "redo" => Action::Redo,
        "repeat" => Action::Repeat,
        "paste" => Action::Paste {
            before: false,
            ripple: true,
            register: None,
        },
        "paste_before" => Action::Paste {
            before: true,
            ripple: true,
            register: None,
        },
        "play_pause" => Action::PlayPause,
        "interrupt_transport" => Action::InterruptTransport,
        "step_frame" | "step_jump_point" => {
            let n = count(arg)?;
            if n == 0 {
                return None;
            }
            let dir = if n > 0 {
                Direction::Forward
            } else {
                Direction::Backward
            };
            let motion = if name == "step_frame" {
                BuiltinMotion::Frame(dir)
            } else {
                BuiltinMotion::JumpPoint(dir)
            };
            Action::Move {
                motion,
                count: n.unsigned_abs().min(u64::from(u32::MAX)) as u32,
            }
        }
        _ => return None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_motion::{BuiltinMotion, Direction};

    #[test]
    fn spec_section_9_2_right_hand_sides_all_parse() {
        assert_eq!(
            parse_editor_command("editor.split_at_playhead"),
            Some(Action::SplitCurrent)
        );
        assert_eq!(
            parse_editor_command("editor.ripple_delete"),
            Some(Action::RippleDeleteClip)
        );
        assert_eq!(
            parse_editor_command("editor.step_frame(-1)"),
            Some(Action::Move {
                motion: BuiltinMotion::Frame(Direction::Backward),
                count: 1
            })
        );
        assert_eq!(
            parse_editor_command("editor.step_frame(1)"),
            Some(Action::Move {
                motion: BuiltinMotion::Frame(Direction::Forward),
                count: 1
            })
        );
    }

    #[test]
    fn an_unknown_or_degenerate_command_is_rejected() {
        assert_eq!(parse_editor_command("editor.frobnicate"), None);
        assert_eq!(parse_editor_command("editor.step_frame(0)"), None);
        assert_eq!(parse_editor_command("editor.step_frame(x)"), None);
    }

    #[test]
    fn opts_expose_typed_scalars() {
        let mut o = Opts::new();
        o.insert("track", OptValue::Str("A2".into()));
        o.insert("threshold_db", OptValue::Num(-2.0));
        assert_eq!(o.str("track"), Some("A2"));
        assert_eq!(o.num("threshold_db"), Some(-2.0));
        assert_eq!(o.num("track"), None);
        assert_eq!(o.get("missing"), None);
        assert_eq!(o.iter().count(), 2);
        assert_eq!(OptValue::Bool(true).to_string(), "true");
    }
}
