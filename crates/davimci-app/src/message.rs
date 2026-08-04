//! Status-line messages and notifications.
//!
//! Phase 0's rule that every error carries a complete user-facing sentence is
//! enforced here by construction: a [`Message`] holds a `String` that came
//! from a typed error's `Display`, never its `Debug`.

use std::collections::VecDeque;

/// How loudly a message is shown. Frontends pick colours from this; the text
/// is identical in all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub severity: Severity,
    pub text: String,
}

impl Message {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            severity: Severity::Info,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            text: text.into(),
        }
    }
}

/// A bounded queue of messages. The newest is what the status line shows; the
/// rest are the `:messages` history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageQueue {
    items: VecDeque<Message>,
    capacity: usize,
}

impl Default for MessageQueue {
    fn default() -> Self {
        Self::with_capacity(200)
    }
}

impl MessageQueue {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    pub fn push(&mut self, message: Message) {
        if self.items.len() == self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(message);
    }

    /// The message the status line shows, if any.
    #[must_use]
    pub fn current(&self) -> Option<&Message> {
        self.items.back()
    }

    /// Oldest first.
    pub fn history(&self) -> impl Iterator<Item = &Message> {
        self.items.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_message_is_current() {
        let mut q = MessageQueue::default();
        q.push(Message::info("first"));
        q.push(Message::error("second"));
        assert_eq!(q.current().map(|m| m.text.as_str()), Some("second"));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn queue_drops_the_oldest_at_capacity() {
        let mut q = MessageQueue::with_capacity(2);
        q.push(Message::info("a"));
        q.push(Message::info("b"));
        q.push(Message::info("c"));
        let texts: Vec<_> = q.history().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, ["b", "c"]);
    }
}
