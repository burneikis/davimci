//! The `:` line: editing, history, and completion (plan.md Phase 9c).
//!
//! State only - no widget, no window. The shell draws the buffer and the
//! caret; what a key does to them is decided here, so the TUI can reuse it
//! verbatim rather than growing a second `:` line.

/// Command-line state.
#[derive(Debug, Clone, Default)]
pub struct CommandLine {
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    /// Index into `history` while browsing with Up/Down.
    browsing: Option<usize>,
    /// Candidate vocabulary for Tab completion, supplied by the host: the
    /// GUI does not own the ex-command vocabulary (`davimci-cli` does).
    candidates: Vec<String>,
}

/// What the shell should do after a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandLineEvent {
    /// Still editing.
    Editing,
    /// The user pressed Enter; run this line.
    Submit(String),
    /// The user pressed Esc, or backspaced past the `:`.
    Cancel,
}

impl CommandLine {
    #[must_use]
    pub fn new(candidates: Vec<String>) -> Self {
        Self {
            candidates,
            ..Self::default()
        }
    }

    pub fn set_candidates(&mut self, candidates: Vec<String>) {
        self.candidates = candidates;
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
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Begin a new `:` line.
    pub fn open(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.browsing = None;
    }

    pub fn insert(&mut self, c: char) -> CommandLineEvent {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
        CommandLineEvent::Editing
    }

    pub fn backspace(&mut self) -> CommandLineEvent {
        if self.cursor == 0 {
            // Backspacing over the `:` leaves the line, like vim.
            return CommandLineEvent::Cancel;
        }
        let prev = self.buffer[..self.cursor]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
        self.cursor -= prev;
        self.buffer.remove(self.cursor);
        CommandLineEvent::Editing
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

    pub fn submit(&mut self) -> CommandLineEvent {
        let line = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.browsing = None;
        if !line.is_empty() && self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }
        CommandLineEvent::Submit(line)
    }

    pub fn cancel(&mut self) -> CommandLineEvent {
        self.buffer.clear();
        self.cursor = 0;
        self.browsing = None;
        CommandLineEvent::Cancel
    }

    /// Up: older history entry. Browsing does not lose the typed line - it is
    /// pushed nowhere until submitted, same as vim.
    pub fn history_prev(&mut self) {
        let next = match self.browsing {
            Some(0) | None if self.history.is_empty() => return,
            None => self.history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.browsing = Some(next);
        if let Some(line) = self.history.get(next) {
            self.buffer = line.clone();
            self.cursor = self.buffer.len();
        }
    }

    /// Down: newer history entry, ending at an empty line.
    pub fn history_next(&mut self) {
        match self.browsing {
            Some(i) if i + 1 < self.history.len() => {
                self.browsing = Some(i + 1);
                if let Some(line) = self.history.get(i + 1) {
                    self.buffer = line.clone();
                    self.cursor = self.buffer.len();
                }
            }
            Some(_) => {
                self.browsing = None;
                self.buffer.clear();
                self.cursor = 0;
            }
            None => {}
        }
    }

    /// Candidates matching the word being typed, in vocabulary order.
    #[must_use]
    pub fn completions(&self) -> Vec<&str> {
        let prefix = self.word();
        self.candidates
            .iter()
            .filter(|c| c.starts_with(prefix))
            .map(String::as_str)
            .collect()
    }

    /// Tab: complete to the longest common prefix of the matches, which is
    /// what shells do and what avoids guessing between two commands.
    pub fn complete(&mut self) -> CommandLineEvent {
        let matches: Vec<String> = self.completions().into_iter().map(str::to_string).collect();
        let Some(first) = matches.first() else {
            return CommandLineEvent::Editing;
        };
        let common = matches.iter().skip(1).fold(first.clone(), |acc, m| {
            let n = acc
                .chars()
                .zip(m.chars())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| a.len_utf8())
                .sum();
            acc[..n].to_string()
        });
        let start = self.buffer.len() - self.word().len();
        self.buffer.truncate(start);
        self.buffer.push_str(&common);
        self.cursor = self.buffer.len();
        CommandLineEvent::Editing
    }

    /// The word under completion: everything after the last space.
    fn word(&self) -> &str {
        match self.buffer.rfind(' ') {
            Some(i) => &self.buffer[i + 1..],
            None => &self.buffer,
        }
    }
}

/// The spec §12 vocabulary, as a default candidate list for a host that has
/// not supplied one.
#[must_use]
pub fn default_candidates() -> Vec<String> {
    [
        "w", "q", "q!", "wq", "x", "e", "new", "ls", "bn", "bp", "b", "relink", "analyze",
        "export", "render",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line() -> CommandLine {
        CommandLine::new(default_candidates())
    }

    #[test]
    fn typing_and_submitting_records_history() {
        let mut c = line();
        c.open();
        for ch in "wq".chars() {
            c.insert(ch);
        }
        assert_eq!(c.submit(), CommandLineEvent::Submit("wq".into()));
        assert_eq!(c.history(), ["wq"]);
    }

    #[test]
    fn backspacing_past_the_colon_cancels() {
        let mut c = line();
        c.open();
        c.insert('w');
        assert_eq!(c.backspace(), CommandLineEvent::Editing);
        assert_eq!(c.backspace(), CommandLineEvent::Cancel);
    }

    #[test]
    fn history_browses_backwards_then_forwards_to_an_empty_line() {
        let mut c = line();
        for cmd in ["w", "ls"] {
            c.open();
            for ch in cmd.chars() {
                c.insert(ch);
            }
            c.submit();
        }
        c.open();
        c.history_prev();
        assert_eq!(c.buffer(), "ls");
        c.history_prev();
        assert_eq!(c.buffer(), "w");
        c.history_next();
        assert_eq!(c.buffer(), "ls");
        c.history_next();
        assert_eq!(c.buffer(), "");
    }

    #[test]
    fn duplicate_consecutive_commands_are_recorded_once() {
        let mut c = line();
        for _ in 0..2 {
            c.open();
            c.insert('w');
            c.submit();
        }
        assert_eq!(c.history(), ["w"]);
    }

    #[test]
    fn tab_completes_to_the_longest_common_prefix() {
        let mut c = line();
        c.open();
        c.insert('b');
        // `b`, `bn`, `bp` share only `b`.
        c.complete();
        assert_eq!(c.buffer(), "b");
        c.insert('n');
        c.complete();
        assert_eq!(c.buffer(), "bn");
    }

    #[test]
    fn completion_applies_to_the_word_under_the_cursor_only() {
        let mut c = CommandLine::new(vec!["h264-1080p".into(), "h264-720p".into()]);
        c.open();
        for ch in "render h264-".chars() {
            c.insert(ch);
        }
        c.complete();
        assert_eq!(c.buffer(), "render h264-");
        assert_eq!(c.completions().len(), 2);
    }

    #[test]
    fn an_unknown_prefix_completes_to_nothing_and_does_not_truncate() {
        let mut c = line();
        c.open();
        for ch in "zzz".chars() {
            c.insert(ch);
        }
        c.complete();
        assert_eq!(c.buffer(), "zzz");
    }

    #[test]
    fn cursor_movement_inserts_in_the_middle() {
        let mut c = line();
        c.open();
        for ch in "wq".chars() {
            c.insert(ch);
        }
        c.left();
        c.insert('!');
        assert_eq!(c.buffer(), "w!q");
        c.right();
        assert_eq!(c.cursor(), 3);
    }
}
