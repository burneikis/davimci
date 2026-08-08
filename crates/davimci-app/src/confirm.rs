//! Yes/no questions the app must ask before something may happen.
//!
//! A question is view state with a consequence: the host raises it, the user
//! answers it in whatever frontend is running, and the host acts on the
//! answer. Keeping it here is what lets the window ask about project-local
//! config instead of the terminal the window was launched from.

use crate::modal::ModalKey;

/// Identifies one question, so an answer cannot be applied to a later one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfirmId(pub u64);

/// A question waiting for a yes or a no.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub id: ConfirmId,
    /// A complete user-facing sentence, ending in the question itself.
    pub question: String,
}

impl Confirm {
    #[must_use]
    pub fn new(id: u64, question: impl Into<String>) -> Self {
        Self {
            id: ConfirmId(id),
            question: question.into(),
        }
    }
}

/// How one keystroke answers, or `None` when the key means nothing here and
/// the question stays up.
///
/// Only `y` is a yes. Every other answer, including the one a stray `Enter`
/// gives, is a no: these questions guard running code the user did not
/// write, so the safe answer must be the easy one.
#[must_use]
pub fn answer_of(key: ModalKey) -> Option<bool> {
    match key {
        ModalKey::Char('y' | 'Y') => Some(true),
        ModalKey::Char('n' | 'N') | ModalKey::Escape | ModalKey::Enter => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_y_is_a_yes() {
        assert_eq!(answer_of(ModalKey::Char('y')), Some(true));
        assert_eq!(answer_of(ModalKey::Char('Y')), Some(true));
        for key in [
            ModalKey::Char('n'),
            ModalKey::Escape,
            ModalKey::Enter,
            ModalKey::Char('x'),
        ] {
            assert_ne!(answer_of(key), Some(true), "{key:?} granted trust");
        }
    }

    /// A key with no meaning leaves the question up rather than answering it
    /// by accident.
    #[test]
    fn an_unrelated_key_does_not_answer() {
        assert_eq!(answer_of(ModalKey::Tab), None);
        assert_eq!(answer_of(ModalKey::Left), None);
    }
}
