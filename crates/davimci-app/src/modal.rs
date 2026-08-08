//! Modal input routing: the `:` line, the media picker, and INSERT-mode
//! subtitle editing.
//!
//! A modal owns the keyboard while it is open, and the key grammar must not
//! see the keystrokes it swallows. Deciding *which* modal owns a key is view
//! logic, so it lives here and not in a frontend: the GUI and the TUI map
//! their raw keys onto [`ModalKey`] and route them through the same table,
//! which is the only way the two can stay in step.

use std::path::{Path, PathBuf};

use davimci_keys::MediaIntent;

use crate::browse::list_dir;
use crate::cmdline::CommandKey;
use crate::frontend::{Event, Response};
use crate::panel::PanelId;
use crate::picker::{Entry, MediaPicker, PickerEvent, PickerIntent};
use crate::subtitle::{SubtitleEdit, SubtitleEvent};
use crate::view::ViewState;

/// A keystroke as a modal understands it: the small alphabet every frontend
/// can supply, whether it reads a window or a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKey {
    Char(char),
    Escape,
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
}

/// The open modals of one frontend.
#[derive(Debug)]
pub struct Modals {
    picker: Option<MediaPicker>,
    /// Where the picker is looking. Remembered between opens, so a second
    /// `i` starts where the last one left off.
    browse_dir: PathBuf,
    subtitle: Option<SubtitleEdit>,
    /// Whether the `:` line is open. The line itself lives in the app; a
    /// frontend only needs to know that the keyboard belongs to it.
    command_open: bool,
    /// The plugin panel that has focus, read from the view. A panel is the
    /// last modal asked, so an editor modal is never taken over by a plugin.
    panel: Option<PanelId>,
}

impl Default for Modals {
    fn default() -> Self {
        Self {
            picker: None,
            browse_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            subtitle: None,
            command_open: false,
            panel: None,
        }
    }
}

impl Modals {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn picker(&self) -> Option<&MediaPicker> {
        self.picker.as_ref()
    }

    #[must_use]
    pub fn subtitle(&self) -> Option<&SubtitleEdit> {
        self.subtitle.as_ref()
    }

    #[must_use]
    pub fn command_is_open(&self) -> bool {
        self.command_open
    }

    /// The focused plugin panel, if one is open.
    #[must_use]
    pub fn panel(&self) -> Option<PanelId> {
        self.panel
    }

    /// True while some modal owns the keyboard.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.picker.is_some()
            || self.subtitle.is_some()
            || self.command_open
            || self.panel.is_some()
    }

    pub fn open_command_line(&mut self) {
        self.command_open = true;
    }

    pub fn open_picker(&mut self, picker: MediaPicker) {
        self.picker = Some(picker);
    }

    pub fn open_subtitle(&mut self, edit: SubtitleEdit) {
        self.subtitle = Some(edit);
    }

    /// Open the picker on `dir` for `intent`. This is the production opener:
    /// `i`/`a`/`r` reach it through [`Response::OpenPicker`].
    pub fn open_picker_at(&mut self, intent: PickerIntent, dir: &Path) {
        self.browse_dir = dir.to_path_buf();
        self.picker = Some(MediaPicker::new(intent, entries_for(dir)));
    }

    /// React to what the app decided. A frontend that ignored this would
    /// simply never show a picker.
    pub fn apply_response(&mut self, response: &Response) {
        match response {
            Response::OpenPicker(intent) => {
                let dir = self.browse_dir.clone();
                self.open_picker_at(intent_of(*intent), &dir);
            }
            // `i` on a subtitle clip edits its text rather than opening a
            // picker.
            Response::EditText { clip, text } => {
                self.subtitle = Some(SubtitleEdit::new(*clip, text.clone()));
            }
            Response::OpenCommandLine => self.command_open = true,
            _ => {}
        }
    }

    /// Take the view's word for whether `COMMAND` mode is live, rather than
    /// guessing from the keys that went past.
    pub fn sync(&mut self, view: &ViewState) {
        self.command_open = view.command_line.is_some();
        // Panels are drawn back to front, so the last focused one is the
        // one on top - the same panel the app hands keys to.
        self.panel = view.panels.iter().rev().find(|p| p.focus).map(|p| p.id);
    }

    /// Route one keystroke.
    ///
    /// `None` means no modal wanted it, so the frontend should translate it
    /// into a grammar key. `Some` is the - possibly empty - list of app
    /// events the keystroke produced; empty means the modal consumed it
    /// silently, which is how browsing a picker looks from outside.
    pub fn handle(&mut self, key: ModalKey) -> Option<Vec<Event>> {
        if self.picker.is_some() {
            return Some(self.handle_picker(key));
        }
        if self.subtitle.is_some() {
            return Some(self.handle_subtitle(key));
        }
        if self.command_open {
            return Some(vec![Event::CommandKey(self.handle_command(key))]);
        }
        // Last, so a plugin panel can never take the keyboard from an
        // editor modal that is already open.
        if let Some(panel) = self.panel {
            if key == ModalKey::Escape {
                self.panel = None;
            }
            return Some(vec![Event::PanelKey { panel, key }]);
        }
        None
    }

    fn handle_picker(&mut self, key: ModalKey) -> Vec<Event> {
        let Some(picker) = self.picker.as_mut() else {
            return Vec::new();
        };
        let event = match key {
            ModalKey::Escape => picker.cancel(),
            ModalKey::Enter => picker.confirm(),
            ModalKey::Backspace => picker.backspace(),
            ModalKey::Down => picker.select_next(),
            ModalKey::Up => picker.select_prev(),
            ModalKey::Char(c) => picker.type_char(c),
            ModalKey::Tab | ModalKey::Left | ModalKey::Right => PickerEvent::Browsing,
        };
        match event {
            PickerEvent::Browsing => Vec::new(),
            // Walking into a directory re-lists it in place; the app is not
            // told, because nothing has been chosen yet.
            PickerEvent::Descend(path) => {
                let dir = PathBuf::from(path);
                self.browse_to(&dir);
                Vec::new()
            }
            PickerEvent::Cancelled => {
                self.picker = None;
                vec![Event::PickerCancelled]
            }
            PickerEvent::Chosen { path, .. } => {
                self.picker = None;
                vec![Event::MediaChosen(PathBuf::from(path))]
            }
        }
    }

    fn handle_subtitle(&mut self, key: ModalKey) -> Vec<Event> {
        let Some(edit) = self.subtitle.as_mut() else {
            return Vec::new();
        };
        let event = match key {
            ModalKey::Escape => edit.commit(),
            ModalKey::Enter => edit.newline(),
            ModalKey::Backspace => edit.backspace(),
            ModalKey::Char(c) => edit.insert(c),
            ModalKey::Left => {
                edit.left();
                SubtitleEvent::Editing
            }
            ModalKey::Right => {
                edit.right();
                SubtitleEvent::Editing
            }
            ModalKey::Tab | ModalKey::Up | ModalKey::Down => SubtitleEvent::Editing,
        };
        match event {
            SubtitleEvent::Editing => Vec::new(),
            // An edit that ends equal to the original commits nothing at all
            //, so the app is told it was abandoned.
            SubtitleEvent::Unchanged => {
                self.subtitle = None;
                vec![Event::TextEditCancelled]
            }
            SubtitleEvent::Commit { clip, text } => {
                self.subtitle = None;
                vec![Event::TextEdited { clip, text }]
            }
        }
    }

    fn handle_command(&mut self, key: ModalKey) -> CommandKey {
        let key = match key {
            ModalKey::Escape => CommandKey::Cancel,
            ModalKey::Enter => CommandKey::Submit,
            ModalKey::Backspace => CommandKey::Backspace,
            ModalKey::Tab => CommandKey::Tab,
            ModalKey::Up => CommandKey::Up,
            ModalKey::Down => CommandKey::Down,
            ModalKey::Left => CommandKey::Left,
            ModalKey::Right => CommandKey::Right,
            ModalKey::Char(c) => CommandKey::Char(c),
        };
        if matches!(key, CommandKey::Cancel | CommandKey::Submit) {
            self.command_open = false;
        }
        key
    }

    /// Re-list the picker at `dir`, keeping the intent it was opened with.
    fn browse_to(&mut self, dir: &Path) {
        self.browse_dir = dir.to_path_buf();
        let entries = entries_for(dir);
        if let Some(p) = self.picker.as_mut() {
            p.set_entries(entries);
        }
    }
}

/// The app's intent, in the picker's vocabulary. Two enums rather than one
/// because `davimci-keys` may not depend on the view layer.
#[must_use]
pub fn intent_of(intent: MediaIntent) -> PickerIntent {
    match intent {
        MediaIntent::Insert => PickerIntent::Insert,
        MediaIntent::Append => PickerIntent::Append,
        MediaIntent::Replace => PickerIntent::Replace,
    }
}

/// The picker's rows for a directory, from the shared listing so the GUI and
/// the TUI show the same files in the same order.
fn entries_for(dir: &Path) -> Vec<Entry> {
    list_dir(dir)
        .into_iter()
        .map(|e| Entry {
            path: e.path.to_string_lossy().to_string(),
            // Keep the browser's label: deriving it from the path would turn
            // `..` into the parent directory's name.
            label: e.label,
            is_dir: e.is_dir,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_open_means_the_grammar_gets_the_key() {
        let mut m = Modals::new();
        assert_eq!(m.handle(ModalKey::Char('d')), None);
    }

    #[test]
    fn the_command_line_owns_the_keyboard_until_it_closes() {
        let mut m = Modals::new();
        m.open_command_line();
        assert_eq!(
            m.handle(ModalKey::Char('w')),
            Some(vec![Event::CommandKey(CommandKey::Char('w'))])
        );
        assert_eq!(
            m.handle(ModalKey::Enter),
            Some(vec![Event::CommandKey(CommandKey::Submit)])
        );
        assert_eq!(m.handle(ModalKey::Char('x')), None, "keys leaked");
    }

    #[test]
    fn the_picker_swallows_keys_until_something_is_chosen() {
        let mut m = Modals::new();
        m.open_picker(MediaPicker::new(
            PickerIntent::Insert,
            vec![Entry::file("/m/a.mkv")],
        ));
        assert_eq!(m.handle(ModalKey::Char('a')), Some(Vec::new()));
        assert_eq!(m.picker().map(|p| p.query().to_string()), Some("a".into()));
        assert_eq!(
            m.handle(ModalKey::Enter),
            Some(vec![Event::MediaChosen(PathBuf::from("/m/a.mkv"))])
        );
        assert!(m.picker().is_none());
    }

    #[test]
    fn an_unchanged_subtitle_edit_commits_nothing() {
        let mut m = Modals::new();
        m.open_subtitle(SubtitleEdit::new(davimci_core::ClipId(1), "hi"));
        assert_eq!(
            m.handle(ModalKey::Escape),
            Some(vec![Event::TextEditCancelled])
        );
        assert!(m.subtitle().is_none());
    }
}
