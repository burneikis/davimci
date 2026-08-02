//! INSERT-mode text editing for subtitle clips (plan.md Phase 9c, spec §8).
//!
//! A subtitle clip's text is timeline content, so committing an edit must go
//! through a `Command`. This buffer therefore never writes anywhere: it holds
//! the in-progress text and reports it on commit, and the app turns that into
//! the edit. Cancelling leaves the timeline byte-identical.

use davimci_core::ClipId;

/// An open text edit over one subtitle clip.
#[derive(Debug, Clone)]
pub struct SubtitleEdit {
    clip: ClipId,
    original: String,
    buffer: String,
    cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtitleEvent {
    Editing,
    /// Esc: the host should run the `SetClipText` edit if the text changed,
    /// and nothing at all if it did not.
    Commit {
        clip: ClipId,
        text: String,
    },
    /// The text is unchanged; nothing to commit.
    Unchanged,
}

impl SubtitleEdit {
    #[must_use]
    pub fn new(clip: ClipId, text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            clip,
            cursor: text.len(),
            buffer: text.clone(),
            original: text,
        }
    }

    #[must_use]
    pub fn clip(&self) -> ClipId {
        self.clip
    }

    #[must_use]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.buffer != self.original
    }

    pub fn insert(&mut self, c: char) -> SubtitleEvent {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        SubtitleEvent::Editing
    }

    pub fn newline(&mut self) -> SubtitleEvent {
        self.insert('\n')
    }

    pub fn backspace(&mut self) -> SubtitleEvent {
        if self.cursor == 0 {
            return SubtitleEvent::Editing;
        }
        let prev = self.buffer[..self.cursor]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
        self.cursor -= prev;
        self.buffer.remove(self.cursor);
        SubtitleEvent::Editing
    }

    pub fn left(&mut self) {
        if let Some(c) = self.buffer[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(c) = self.buffer[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Esc.
    #[must_use]
    pub fn commit(&self) -> SubtitleEvent {
        if self.is_dirty() {
            SubtitleEvent::Commit {
                clip: self.clip,
                text: self.buffer.clone(),
            }
        } else {
            SubtitleEvent::Unchanged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit() -> SubtitleEdit {
        SubtitleEdit::new(ClipId(3), "hello")
    }

    #[test]
    fn an_untouched_edit_commits_nothing() {
        assert_eq!(edit().commit(), SubtitleEvent::Unchanged);
    }

    #[test]
    fn typing_commits_the_new_text_for_the_clip() {
        let mut e = edit();
        e.insert('!');
        assert_eq!(
            e.commit(),
            SubtitleEvent::Commit {
                clip: ClipId(3),
                text: "hello!".into()
            }
        );
    }

    #[test]
    fn editing_back_to_the_original_text_commits_nothing() {
        let mut e = edit();
        e.insert('!');
        e.backspace();
        assert_eq!(e.commit(), SubtitleEvent::Unchanged);
    }

    #[test]
    fn multibyte_text_edits_on_character_boundaries() {
        let mut e = SubtitleEdit::new(ClipId(1), "héllo");
        e.left();
        e.left();
        e.left();
        e.left();
        e.backspace();
        assert_eq!(e.buffer(), "éllo");
    }

    #[test]
    fn newline_is_a_character_not_a_commit() {
        let mut e = edit();
        assert_eq!(e.newline(), SubtitleEvent::Editing);
        assert_eq!(e.buffer(), "hello\n");
    }
}
