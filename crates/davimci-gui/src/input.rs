//! Key translation (plan.md Phase 9c): window events in, `davimci-keys`
//! tokens out, and nothing else.
//!
//! The raw key model here is deliberately not `winit`'s. It is the small
//! subset davimci binds, so translation is testable with no window and the
//! same table serves a `winit` shell, a test, and (with a different adapter)
//! a terminal. A shell's job is to fill in [`RawKey`]; it may not decide what
//! a key means.

use davimci_keys::{Key, Named};

/// Modifier state at the moment of a key press.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
}

impl Modifiers {
    #[must_use]
    pub fn ctrl() -> Self {
        Self {
            ctrl: true,
            ..Self::default()
        }
    }
}

/// A physical key press, as much as davimci cares about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawKey {
    /// A text-producing key, already shifted by the platform's keyboard
    /// layout - `Shift`+`v` arrives as `'V'`, which is why `Modifiers::shift`
    /// is ignored for character keys.
    Char(char),
    Escape,
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Space,
    /// Anything davimci does not bind.
    Other,
}

/// Translate one key press.
///
/// Returns `None` for keys with no binding, so a shell can pass them to a
/// text field instead of inventing a token.
#[must_use]
pub fn translate(key: &RawKey, mods: Modifiers) -> Option<Key> {
    if mods.logo {
        // Super is the window manager's, never the editor's.
        return None;
    }
    match key {
        RawKey::Char(c) => {
            if mods.ctrl {
                Some(Key::Ctrl(c.to_ascii_lowercase()))
            } else if mods.alt {
                None
            } else {
                Some(Key::Char(*c))
            }
        }
        RawKey::Space => Some(Key::Named(Named::Space)),
        RawKey::Escape => Some(Key::Named(Named::Esc)),
        RawKey::Enter => Some(Key::Named(Named::Enter)),
        RawKey::Backspace => Some(Key::Named(Named::Backspace)),
        RawKey::Tab => Some(Key::Named(Named::Tab)),
        RawKey::Left => Some(Key::Named(Named::Left)),
        RawKey::Right => Some(Key::Named(Named::Right)),
        RawKey::Up => Some(Key::Named(Named::Up)),
        RawKey::Down => Some(Key::Named(Named::Down)),
        RawKey::Other => None,
    }
}

/// Translate a whole sequence, dropping unbound keys.
#[must_use]
pub fn translate_all(keys: &[(RawKey, Modifiers)]) -> Vec<Key> {
    keys.iter().filter_map(|(k, m)| translate(k, *m)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Modifiers {
        Modifiers::default()
    }

    #[test]
    fn characters_pass_through_already_shifted() {
        assert_eq!(translate(&RawKey::Char('v'), none()), Some(Key::Char('v')));
        let shifted = Modifiers {
            shift: true,
            ..none()
        };
        assert_eq!(
            translate(&RawKey::Char('V'), shifted),
            Some(Key::Char('V')),
            "shift must not be applied twice"
        );
    }

    #[test]
    fn ctrl_v_is_the_visual_block_token() {
        assert_eq!(
            translate(&RawKey::Char('v'), Modifiers::ctrl()),
            Some(Key::Ctrl('v'))
        );
        // Ctrl+Shift+V still names `Ctrl-v`: the grammar has no shifted
        // control tokens.
        let m = Modifiers {
            ctrl: true,
            shift: true,
            ..none()
        };
        assert_eq!(translate(&RawKey::Char('V'), m), Some(Key::Ctrl('v')));
    }

    #[test]
    fn named_keys_map_to_their_tokens() {
        for (raw, named) in [
            (RawKey::Escape, Named::Esc),
            (RawKey::Left, Named::Left),
            (RawKey::Right, Named::Right),
            (RawKey::Space, Named::Space),
            (RawKey::Enter, Named::Enter),
        ] {
            assert_eq!(translate(&raw, none()), Some(Key::Named(named)));
        }
    }

    #[test]
    fn unbound_and_super_chorded_keys_are_dropped() {
        assert_eq!(translate(&RawKey::Other, none()), None);
        let logo = Modifiers {
            logo: true,
            ..none()
        };
        assert_eq!(translate(&RawKey::Char('l'), logo), None);
        let alt = Modifiers {
            alt: true,
            ..none()
        };
        assert_eq!(translate(&RawKey::Char('l'), alt), None);
    }

    #[test]
    fn a_sequence_translates_to_the_same_tokens_the_parser_takes() {
        let typed = [
            (RawKey::Char('3'), none()),
            (RawKey::Char('d'), none()),
            (RawKey::Char('w'), none()),
        ];
        assert_eq!(translate_all(&typed), Key::parse_str("3dw"));
    }

    #[test]
    fn arrow_keys_translate_to_the_one_frame_motions() {
        let typed = [(RawKey::Left, none()), (RawKey::Right, none())];
        assert_eq!(translate_all(&typed), Key::parse_str("<Left><Right>"));
    }
}
