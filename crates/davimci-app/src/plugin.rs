//! What a plugin asked for, in terms the app can execute.
//!
//! The Lua runtime lives in the host binary, and the write path lives in the
//! key engine, which the app owns. This is the type that crosses between
//! them: a host turns Lua requests into [`PluginEffects`], and the app runs
//! each action through the same `Session::exec` a keystroke would, so a
//! plugin edit is an ordinary undo-tree entry.

use davimci_keys::Action;

use crate::message::Message;
use crate::panel::PanelOp;

/// Actions to run and things to say, in the order the plugin asked for them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginEffects {
    pub actions: Vec<Action>,
    pub messages: Vec<Message>,
    /// Panels to open, fill, show, hide or close. Applied after the actions,
    /// so a plugin that edits and then reports sees the edit's result.
    pub panels: Vec<PanelOp>,
}

impl PluginEffects {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.messages.is_empty() && self.panels.is_empty()
    }

    pub fn panel(&mut self, op: PanelOp) {
        self.panels.push(op);
    }

    pub fn say(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn act(&mut self, action: Action) {
        self.actions.push(action);
    }

    /// Report whatever a request came back with: its own sentence either way,
    /// so a plugin failure reads like any other status line.
    pub fn report<E: std::fmt::Display>(&mut self, result: Result<String, E>) {
        match result {
            Ok(text) => self.say(Message::info(text)),
            Err(e) => self.say(Message::error(e.to_string())),
        }
    }
}
