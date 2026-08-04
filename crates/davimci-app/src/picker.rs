//! The media picker behind `i` / `a` / `r` (spec 11).
//!
//! State only: a frontend draws a list and the host supplies its entries. It
//! lives here rather than in a frontend because the GUI and the TUI both
//! open it, and two copies would be two pickers. The crate does no I/O, so
//! the picker stays testable and the "only the CLI layer touches the
//! filesystem" rule holds.

/// One selectable entry, as the host described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub label: String,
    pub is_dir: bool,
}

impl Entry {
    #[must_use]
    pub fn file(path: impl Into<String>) -> Self {
        let path = path.into();
        let label = path.rsplit('/').next().unwrap_or(&path).to_string();
        Self {
            path,
            label,
            is_dir: false,
        }
    }

    #[must_use]
    pub fn dir(path: impl Into<String>) -> Self {
        let mut e = Self::file(path);
        e.is_dir = true;
        e
    }
}

/// Which verb opened the picker; the app needs it back when one is chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerIntent {
    /// `i`: insert at the playhead.
    Insert,
    /// `a`: append after the current clip.
    Append,
    /// `r`: replace the clip under the playhead.
    Replace,
}

/// A filtered, keyboard-driven list.
#[derive(Debug, Clone)]
pub struct MediaPicker {
    intent: PickerIntent,
    entries: Vec<Entry>,
    query: String,
    selected: usize,
}

/// What a key did to the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerEvent {
    Browsing,
    /// The user chose this path for `intent`.
    Chosen {
        intent: PickerIntent,
        path: String,
    },
    /// The user chose a directory; the host should list it and call
    /// [`MediaPicker::set_entries`].
    Descend(String),
    Cancelled,
}

impl MediaPicker {
    #[must_use]
    pub fn new(intent: PickerIntent, entries: Vec<Entry>) -> Self {
        Self {
            intent,
            entries,
            query: String::new(),
            selected: 0,
        }
    }

    #[must_use]
    pub fn intent(&self) -> PickerIntent {
        self.intent
    }

    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.query.clear();
        self.selected = 0;
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Entries matching the query, case-insensitively, in host order.
    #[must_use]
    pub fn visible(&self) -> Vec<&Entry> {
        if self.query.is_empty() {
            return self.entries.iter().collect();
        }
        let q = self.query.to_lowercase();
        self.entries
            .iter()
            .filter(|e| e.label.to_lowercase().contains(&q))
            .collect()
    }

    /// Index into [`MediaPicker::visible`], always in range when it is
    /// non-empty.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected.min(self.visible().len().saturating_sub(1))
    }

    #[must_use]
    pub fn selected_entry(&self) -> Option<&Entry> {
        let visible = self.visible();
        visible
            .get(self.selected.min(visible.len().saturating_sub(1)))
            .copied()
    }

    pub fn type_char(&mut self, c: char) -> PickerEvent {
        self.query.push(c);
        self.selected = 0;
        PickerEvent::Browsing
    }

    pub fn backspace(&mut self) -> PickerEvent {
        self.query.pop();
        self.selected = 0;
        PickerEvent::Browsing
    }

    /// `j` / Down.
    pub fn select_next(&mut self) -> PickerEvent {
        let n = self.visible().len();
        if n > 0 {
            self.selected = (self.selected() + 1) % n;
        }
        PickerEvent::Browsing
    }

    /// `k` / Up.
    pub fn select_prev(&mut self) -> PickerEvent {
        let n = self.visible().len();
        if n > 0 {
            self.selected = (self.selected() + n - 1) % n;
        }
        PickerEvent::Browsing
    }

    pub fn confirm(&mut self) -> PickerEvent {
        match self.selected_entry() {
            Some(e) if e.is_dir => PickerEvent::Descend(e.path.clone()),
            Some(e) => PickerEvent::Chosen {
                intent: self.intent,
                path: e.path.clone(),
            },
            None => PickerEvent::Browsing,
        }
    }

    pub fn cancel(&mut self) -> PickerEvent {
        PickerEvent::Cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picker() -> MediaPicker {
        MediaPicker::new(
            PickerIntent::Insert,
            vec![
                Entry::dir("/media/clips"),
                Entry::file("/media/bunny.mkv"),
                Entry::file("/media/Interview.mov"),
            ],
        )
    }

    #[test]
    fn filtering_is_case_insensitive_and_resets_the_selection() {
        let mut p = picker();
        p.select_next();
        p.type_char('i');
        let visible: Vec<&str> = p.visible().iter().map(|e| e.label.as_str()).collect();
        assert_eq!(visible, ["clips", "Interview.mov"]);
        assert_eq!(p.selected(), 0);
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut p = picker();
        p.select_prev();
        assert_eq!(
            p.selected_entry().map(|e| e.label.as_str()),
            Some("Interview.mov")
        );
        p.select_next();
        assert_eq!(p.selected_entry().map(|e| e.label.as_str()), Some("clips"));
    }

    #[test]
    fn choosing_a_file_reports_the_intent_that_opened_the_picker() {
        let mut p = MediaPicker::new(PickerIntent::Replace, vec![Entry::file("/m/a.mkv")]);
        assert_eq!(
            p.confirm(),
            PickerEvent::Chosen {
                intent: PickerIntent::Replace,
                path: "/m/a.mkv".into()
            }
        );
    }

    #[test]
    fn choosing_a_directory_asks_the_host_to_list_it() {
        let mut p = picker();
        assert_eq!(p.confirm(), PickerEvent::Descend("/media/clips".into()));
    }

    #[test]
    fn an_empty_filter_result_confirms_nothing() {
        let mut p = picker();
        for c in "zzz".chars() {
            p.type_char(c);
        }
        assert!(p.visible().is_empty());
        assert_eq!(p.confirm(), PickerEvent::Browsing);
        p.backspace();
        assert_eq!(p.visible().len(), 0);
    }
}
