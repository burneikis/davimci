//! Everything user Lua registered, and nothing it can mutate directly.

use std::collections::BTreeMap;
use std::fmt;

use davimci_keys::{Key, Mode};
use mlua::Function;

use crate::config::TimelineConfig;
use crate::preset::ExportPreset;
use crate::request::Request;

/// Identifies a callback the host must ask the runtime to invoke.
pub type HandlerId = u32;

/// A keymap right-hand side (spec §9.2): either a named editor command or a
/// Lua function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rhs {
    /// An `editor.*` string, already validated at `map()` time.
    Command(String),
    /// A Lua function, invoked through [`crate::Runtime::invoke`].
    Callback(HandlerId),
}

/// One `map(mode, lhs, rhs)` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub mode: Mode,
    pub keys: Vec<Key>,
    pub rhs: Rhs,
}

/// A `davimci.textobject.register` definition (spec §9.4).
pub struct ObjectDef {
    pub name: String,
    pub inner: Option<Function>,
    pub around: Option<Function>,
}

impl fmt::Debug for ObjectDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectDef")
            .field("name", &self.name)
            .field("inner", &self.inner.is_some())
            .field("around", &self.around.is_some())
            .finish()
    }
}

/// One registered event handler.
pub struct Autocmd {
    pub id: HandlerId,
    pub event: String,
    pub func: Function,
    /// Cleared when the handler throws; a broken handler is disabled for the
    /// session rather than being allowed to break the editor (Phase 0).
    pub enabled: bool,
}

impl fmt::Debug for Autocmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Autocmd")
            .field("id", &self.id)
            .field("event", &self.event)
            .field("enabled", &self.enabled)
            .finish()
    }
}

/// The shared state the `davimci.*` modules write into.
#[derive(Default)]
pub(crate) struct State {
    pub keymaps: Vec<KeyBinding>,
    pub motions: BTreeMap<String, Function>,
    pub objects: BTreeMap<String, ObjectDef>,
    pub presets: BTreeMap<String, ExportPreset>,
    pub timeline: TimelineConfig,
    pub callbacks: BTreeMap<HandlerId, Function>,
    pub autocmds: Vec<Autocmd>,
    pub requests: Vec<Request>,
    next_id: HandlerId,
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("keymaps", &self.keymaps)
            .field("motions", &self.motions.keys().collect::<Vec<_>>())
            .field("objects", &self.objects.keys().collect::<Vec<_>>())
            .field("presets", &self.presets.keys().collect::<Vec<_>>())
            .field("timeline", &self.timeline)
            .field("autocmds", &self.autocmds)
            .field("requests", &self.requests)
            .finish()
    }
}

impl State {
    pub fn next_id(&mut self) -> HandlerId {
        self.next_id += 1;
        self.next_id
    }
}

/// Parse the mode name a keymap was registered for.
pub(crate) fn parse_mode(name: &str) -> Option<Mode> {
    Some(match name {
        "normal" | "n" => Mode::Normal,
        "visual" | "v" => Mode::Visual,
        "visual-line" | "V" => Mode::VisualLine,
        "visual-block" => Mode::VisualBlock,
        "insert" | "i" => Mode::Insert,
        "command" | "c" => Mode::Command,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_names_cover_the_fsm() {
        assert_eq!(parse_mode("normal"), Some(Mode::Normal));
        assert_eq!(parse_mode("visual-block"), Some(Mode::VisualBlock));
        assert_eq!(parse_mode("nrmal"), None);
    }

    #[test]
    fn handler_ids_are_unique_and_nonzero() {
        let mut s = State::default();
        let a = s.next_id();
        let b = s.next_id();
        assert_ne!(a, b);
        assert!(a > 0);
    }
}
