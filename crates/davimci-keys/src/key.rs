//! Key tokens (spec §3.2.1, §11).
//!
//! A [`Key`] is one physical keypress, already stripped of platform detail -
//! a frontend translates `winit`/terminal events into these and nothing
//! else, per plan.md Phase 9a/9c/9d. `<Space>` is both a literal key and the
//! spec's leader.

/// Named keys that are not a plain character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Named {
    Space,
    Esc,
    Enter,
    Left,
    Right,
    Up,
    Down,
    Tab,
    Backspace,
}

/// One keypress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    /// A character typed while holding Control.
    Ctrl(char),
    Named(Named),
}

impl Key {
    /// Parse a vim-style key string such as `"3dw"`, `"<C-r>"`, `"<Space>p"`,
    /// into its token sequence. Used by tests and by `:map`-style config.
    #[must_use]
    pub fn parse_str(s: &str) -> Vec<Key> {
        let mut out = Vec::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '<' {
                let mut tag = String::new();
                let mut closed = false;
                for c2 in chars.by_ref() {
                    if c2 == '>' {
                        closed = true;
                        break;
                    }
                    tag.push(c2);
                }
                if closed && let Some(k) = parse_tag(&tag) {
                    out.push(k);
                    continue;
                }
                // Not a recognised tag: fall back to literal characters.
                out.push(Key::Char('<'));
                out.extend(Key::parse_str(&tag));
                if closed {
                    out.push(Key::Char('>'));
                }
            } else if c == ' ' {
                out.push(Key::Named(Named::Space));
            } else {
                out.push(Key::Char(c));
            }
        }
        out
    }

    #[must_use]
    pub fn is_esc(self) -> bool {
        matches!(self, Key::Named(Named::Esc))
    }

    #[must_use]
    pub fn as_char(self) -> Option<char> {
        match self {
            Key::Char(c) => Some(c),
            _ => None,
        }
    }

    /// Render back to a token vim/config strings would use, so a macro
    /// buffer (opaque strings, plan.md Phase 2) round-trips through the
    /// parser exactly.
    #[must_use]
    pub fn to_token(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Ctrl(c) => format!("<C-{c}>"),
            Key::Named(Named::Space) => " ".to_string(),
            Key::Named(n) => format!("<{}>", named_tag(n)),
        }
    }
}

fn named_tag(n: Named) -> &'static str {
    match n {
        Named::Space => "Space",
        Named::Esc => "Esc",
        Named::Enter => "CR",
        Named::Left => "Left",
        Named::Right => "Right",
        Named::Up => "Up",
        Named::Down => "Down",
        Named::Tab => "Tab",
        Named::Backspace => "BS",
    }
}

fn parse_tag(tag: &str) -> Option<Key> {
    let lower = tag.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("c-") {
        let c = rest.chars().next()?;
        if rest.chars().count() == 1 {
            return Some(Key::Ctrl(c));
        }
        return None;
    }
    Some(match lower.as_str() {
        "space" | "leader" => Key::Named(Named::Space),
        "esc" | "escape" => Key::Named(Named::Esc),
        "cr" | "enter" | "return" => Key::Named(Named::Enter),
        "left" => Key::Named(Named::Left),
        "right" => Key::Named(Named::Right),
        "up" => Key::Named(Named::Up),
        "down" => Key::Named(Named::Down),
        "tab" => Key::Named(Named::Tab),
        "bs" | "backspace" => Key::Named(Named::Backspace),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_characters_parse_one_per_key() {
        assert_eq!(
            Key::parse_str("3dw"),
            vec![Key::Char('3'), Key::Char('d'), Key::Char('w')]
        );
    }

    #[test]
    fn named_tags_and_ctrl_parse() {
        assert_eq!(Key::parse_str("<C-r>"), vec![Key::Ctrl('r')]);
        assert_eq!(
            Key::parse_str("<Space><Space>"),
            vec![Key::Named(Named::Space), Key::Named(Named::Space)]
        );
        assert_eq!(Key::parse_str(" "), vec![Key::Named(Named::Space)]);
    }

    #[test]
    fn tokens_round_trip() {
        for s in ["3dw", "<C-r>", "<Space>p", "\"ayy", "gs"] {
            let keys = Key::parse_str(s);
            let back: String = keys.iter().map(|k| k.to_token()).collect();
            assert_eq!(Key::parse_str(&back), keys, "{s} did not round-trip");
        }
    }
}
