//! Terminal key translation: crossterm events in, `davimci-keys` tokens out.
//!
//! The raw key model is the same small subset the GUI translates, so the two
//! frontends bind the same alphabet. A terminal adds one wrinkle a window
//! does not have: control chords arrive as `Ctrl` plus the unshifted letter,
//! and some terminals deliver `Ctrl-i` as Tab. What a key *means* is still
//! the app's business; this file only names it.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use davimci_keys::{Key, Named};

/// Modifier state at the moment of a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A key press, as much as davimci cares about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermKey {
    /// A text-producing key, already shifted by the terminal.
    Char(char),
    Escape,
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    /// Anything davimci does not bind.
    Other,
}

/// Translate one key press into a grammar token.
///
/// `None` for keys with no binding, so a caller can leave them to a text
/// field instead of inventing a token.
#[must_use]
pub fn translate(key: &TermKey, mods: Modifiers) -> Option<Key> {
    match key {
        TermKey::Char(c) => {
            if mods.ctrl {
                Some(Key::Ctrl(c.to_ascii_lowercase()))
            } else if mods.alt {
                None
            } else if *c == ' ' {
                Some(Key::Named(Named::Space))
            } else {
                Some(Key::Char(*c))
            }
        }
        TermKey::Escape => Some(Key::Named(Named::Esc)),
        TermKey::Enter => Some(Key::Named(Named::Enter)),
        TermKey::Backspace => Some(Key::Named(Named::Backspace)),
        TermKey::Tab => Some(Key::Named(Named::Tab)),
        TermKey::Left => Some(Key::Named(Named::Left)),
        TermKey::Right => Some(Key::Named(Named::Right)),
        TermKey::Up => Some(Key::Named(Named::Up)),
        TermKey::Down => Some(Key::Named(Named::Down)),
        TermKey::Other => None,
    }
}

/// Name one crossterm key event.
///
/// Key *releases* are dropped: terminals with the kitty protocol report them,
/// and acting on both edges would run every binding twice.
#[must_use]
pub fn from_crossterm(event: &KeyEvent) -> Option<(TermKey, Modifiers)> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let mods = Modifiers {
        ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
        alt: event.modifiers.contains(KeyModifiers::ALT),
        shift: event.modifiers.contains(KeyModifiers::SHIFT),
    };
    let key = match event.code {
        KeyCode::Char(c) => TermKey::Char(c),
        KeyCode::Esc => TermKey::Escape,
        KeyCode::Enter => TermKey::Enter,
        KeyCode::Backspace => TermKey::Backspace,
        KeyCode::Tab | KeyCode::BackTab => TermKey::Tab,
        KeyCode::Left => TermKey::Left,
        KeyCode::Right => TermKey::Right,
        KeyCode::Up => TermKey::Up,
        KeyCode::Down => TermKey::Down,
        _ => TermKey::Other,
    };
    Some((key, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn characters_pass_through_already_shifted() {
        assert_eq!(translate(&TermKey::Char('v'), none()), Some(Key::Char('v')));
        let shifted = Modifiers {
            shift: true,
            ..none()
        };
        assert_eq!(
            translate(&TermKey::Char('V'), shifted),
            Some(Key::Char('V')),
            "shift must not be applied twice"
        );
    }

    #[test]
    fn a_typed_space_is_the_transport_token() {
        assert_eq!(
            translate(&TermKey::Char(' '), none()),
            Some(Key::Named(Named::Space)),
            "a terminal has no Space key of its own; it sends the character"
        );
    }

    #[test]
    fn ctrl_chords_lower_case_the_letter() {
        let ctrl = Modifiers {
            ctrl: true,
            ..none()
        };
        assert_eq!(translate(&TermKey::Char('V'), ctrl), Some(Key::Ctrl('v')));
    }

    #[test]
    fn a_whole_session_translates_to_the_tokens_the_parser_takes() {
        let typed: Vec<Key> = "3dw"
            .chars()
            .filter_map(|c| translate(&TermKey::Char(c), none()))
            .collect();
        assert_eq!(typed, Key::parse_str("3dw"));
    }

    #[test]
    fn key_releases_are_not_key_presses() {
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(from_crossterm(&release), None);
        let press = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(
            from_crossterm(&press),
            Some((TermKey::Char('d'), Modifiers::default()))
        );
    }
}
