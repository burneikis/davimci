//! What a plugin asked for, in terms the app can execute.
//!
//! The Lua runtime lives in the host binary, and the write path lives in the
//! key engine, which the app owns. This is the type that crosses between
//! them: a host turns Lua requests into [`PluginEffects`], and the app runs
//! each action through the same `Session::exec` a keystroke would, so a
//! plugin edit is an ordinary undo-tree entry.

use davimci_keys::Action;

use crate::message::Message;

/// Actions to run and things to say, in the order the plugin asked for them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginEffects {
    pub actions: Vec<Action>,
    pub messages: Vec<Message>,
}

impl PluginEffects {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.messages.is_empty()
    }

    pub fn say(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn act(&mut self, action: Action) {
        self.actions.push(action);
    }
}
