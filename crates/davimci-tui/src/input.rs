//! Terminal key translation: crossterm events in, the shared raw key
//! alphabet out.
//!
//! The alphabet and the translation table live in `davimci_app::rawkey`, so
//! the two frontends cannot bind different keys. A terminal adds one wrinkle
//! a window does not have: control chords arrive as `Ctrl` plus the unshifted
//! letter, and some terminals deliver `Ctrl-i` as Tab. What a key *means* is
//! still the app's business; this file only names it.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub use davimci_app::rawkey::{Modifiers, RawKey as TermKey, translate};

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
        logo: event.modifiers.contains(KeyModifiers::SUPER),
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
    use davimci_keys::Key;

    #[test]
    fn ctrl_chords_lower_case_the_letter() {
        // Terminals report the unshifted letter for control chords; a
        // shifted one must still name the same token.
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(translate(&TermKey::Char('V'), ctrl), Some(Key::Ctrl('v')));
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
