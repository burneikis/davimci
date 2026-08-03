//! The window shell's state machine (plan.md Phase 9c).
//!
//! [`Gui`] is a [`Frontend`]: it turns window events into `davimci-app`
//! events and draws the [`ViewState`] it is handed. Modal input - the `:`
//! line, the media picker, INSERT-mode subtitle editing - is routed here,
//! because a modal owns the keyboard and the key grammar must not see those
//! keystrokes.
//!
//! The windowing layer (not yet written, see the crate docs) does exactly two
//! things: push [`GuiEvent`]s in, and rasterise the [`DrawList`] out.

use davimci_app::{AppError, Event, Frontend, Response, Surface, ViewState};
use davimci_keys::MediaIntent;

use davimci_app::CommandKey;

use crate::input::{Modifiers, RawKey, translate};
use crate::layout::{Layout, Metrics, paint};
use crate::paint::{Chrome, DrawList, PickerRow, PickerView};
use crate::picker::{Entry, MediaPicker, PickerEvent, PickerIntent};
use crate::subtitle::{SubtitleEdit, SubtitleEvent};

/// Something the windowing layer observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiEvent {
    Key(RawKey, Modifiers),
    Resized {
        width: u32,
        height: u32,
    },
    CloseRequested,
    /// A click in the timeline, in window pixels.
    Click {
        x: i32,
        y: i32,
    },
    Redraw,
}

/// The GUI frontend.
#[derive(Debug)]
pub struct Gui {
    width: u32,
    height: u32,
    metrics: Metrics,
    pending: Vec<GuiEvent>,
    out: Vec<Event>,
    /// Whether the `:` line is open. The line itself lives in the app; the
    /// shell only needs to know that the keyboard belongs to it and that the
    /// layout has a row for it.
    command_open: bool,
    /// Whether the last view had suggestions, so the layout keeps a row for
    /// them. Read from the view rather than decided here.
    completions_shown: bool,
    picker: Option<MediaPicker>,
    /// Where the picker is looking. Remembered between opens, so a second
    /// `i` starts where the last one left off.
    browse_dir: std::path::PathBuf,
    subtitle: Option<SubtitleEdit>,
    chrome: Chrome,
    last_draw: Option<DrawList>,
    quit: bool,
}

impl Gui {
    #[must_use]
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            metrics: Metrics::default(),
            pending: Vec::new(),
            out: Vec::new(),
            command_open: false,
            completions_shown: false,
            picker: None,
            browse_dir: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            subtitle: None,
            chrome: Chrome::default(),
            last_draw: None,
            quit: false,
        }
    }

    pub fn push(&mut self, event: GuiEvent) {
        self.pending.push(event);
    }

    pub fn set_chrome(&mut self, chrome: Chrome) {
        self.chrome = chrome;
    }

    pub fn set_metrics(&mut self, metrics: Metrics) {
        self.metrics = metrics;
    }

    #[must_use]
    pub fn layout(&self) -> Layout {
        Layout::compute(
            self.width,
            self.height,
            self.metrics,
            self.command_open,
            self.completions_shown,
        )
    }

    #[must_use]
    pub fn last_draw(&self) -> Option<&DrawList> {
        self.last_draw.as_ref()
    }

    #[must_use]
    pub fn command_is_open(&self) -> bool {
        self.command_open
    }

    #[must_use]
    pub fn picker(&self) -> Option<&MediaPicker> {
        self.picker.as_ref()
    }

    pub fn open_picker(&mut self, picker: MediaPicker) {
        self.picker = Some(picker);
    }

    /// Open the picker on `dir` for `intent`. This is the production opener:
    /// `i`/`a`/`r` reach it through [`Response::OpenPicker`].
    pub fn open_picker_at(&mut self, intent: PickerIntent, dir: &std::path::Path) {
        self.browse_dir = dir.to_path_buf();
        self.picker = Some(MediaPicker::new(intent, entries_for(dir)));
    }

    /// Re-list the picker at `dir`, keeping the intent it was opened with.
    fn browse_to(&mut self, dir: &std::path::Path) {
        self.browse_dir = dir.to_path_buf();
        let entries = entries_for(dir);
        if let Some(p) = self.picker.as_mut() {
            p.set_entries(entries);
        }
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
            // picker (spec §8, §15.4).
            Response::EditText { clip, text } => {
                self.open_subtitle(SubtitleEdit::new(*clip, text.clone()));
            }
            _ => {}
        }
    }

    pub fn open_subtitle(&mut self, edit: SubtitleEdit) {
        self.subtitle = Some(edit);
    }

    #[must_use]
    pub fn subtitle(&self) -> Option<&SubtitleEdit> {
        self.subtitle.as_ref()
    }

    /// The app told us it entered `COMMAND` mode.
    pub fn open_command_line(&mut self) {
        self.command_open = true;
    }

    /// Turn one window event into app events, routing modals first.
    fn handle(&mut self, event: GuiEvent) {
        match event {
            GuiEvent::Resized { width, height } => {
                self.width = width;
                self.height = height;
                let surface = self.layout().surface();
                self.out.push(Event::Resize(surface));
            }
            GuiEvent::CloseRequested => {
                self.quit = true;
                self.out.push(Event::Quit);
            }
            GuiEvent::Redraw => self.out.push(Event::Tick),
            GuiEvent::Click { x, y } => {
                // The shell reports where the click landed; the app decides
                // that it means "seek there". A frontend that decided for
                // itself would be a second editor.
                if let Some(column) = self.column_at(x, y) {
                    self.out.push(Event::Click {
                        column,
                        row: self.lane_at(y),
                    });
                }
            }
            GuiEvent::Key(raw, mods) => self.handle_key(raw, mods),
        }
    }

    fn handle_key(&mut self, raw: RawKey, mods: Modifiers) {
        if let Some(picker) = self.picker.as_mut() {
            let ev = match &raw {
                RawKey::Escape => picker.cancel(),
                RawKey::Enter => picker.confirm(),
                RawKey::Backspace => picker.backspace(),
                RawKey::Down => picker.select_next(),
                RawKey::Up => picker.select_prev(),
                RawKey::Char(c) => picker.type_char(*c),
                _ => PickerEvent::Browsing,
            };
            match ev {
                PickerEvent::Browsing => {}
                // Walking into a directory re-lists it in place; the app is
                // not told, because nothing has been chosen yet.
                PickerEvent::Descend(path) => {
                    let dir = std::path::PathBuf::from(path);
                    self.browse_to(&dir);
                }
                PickerEvent::Cancelled => {
                    self.picker = None;
                    self.out.push(Event::PickerCancelled);
                }
                PickerEvent::Chosen { path, .. } => {
                    self.picker = None;
                    self.out
                        .push(Event::MediaChosen(std::path::PathBuf::from(path)));
                }
            }
            return;
        }

        if let Some(edit) = self.subtitle.as_mut() {
            let ev = match &raw {
                RawKey::Escape => edit.commit(),
                RawKey::Enter => edit.newline(),
                RawKey::Backspace => edit.backspace(),
                RawKey::Char(c) => edit.insert(*c),
                RawKey::Left => {
                    edit.left();
                    SubtitleEvent::Editing
                }
                RawKey::Right => {
                    edit.right();
                    SubtitleEvent::Editing
                }
                _ => SubtitleEvent::Editing,
            };
            match ev {
                SubtitleEvent::Editing => {}
                // An edit that ends equal to the original commits nothing at
                // all (spec §15.4), so the app is told it was abandoned.
                SubtitleEvent::Unchanged => {
                    self.subtitle = None;
                    self.out.push(Event::TextEditCancelled);
                }
                SubtitleEvent::Commit { clip, text } => {
                    self.subtitle = None;
                    self.out.push(Event::TextEdited { clip, text });
                }
            }
            return;
        }

        if self.command_open {
            // The buffer lives in the app: the shell forwards the keystroke
            // and learns whether the line closed from the next view.
            let key = match &raw {
                RawKey::Escape => CommandKey::Cancel,
                RawKey::Enter => CommandKey::Submit,
                RawKey::Backspace => CommandKey::Backspace,
                RawKey::Tab => CommandKey::Tab,
                RawKey::Up => CommandKey::Up,
                RawKey::Down => CommandKey::Down,
                RawKey::Left => CommandKey::Left,
                RawKey::Right => CommandKey::Right,
                RawKey::Space => CommandKey::Char(' '),
                RawKey::Char(c) => CommandKey::Char(*c),
                RawKey::Other => return,
            };
            if matches!(key, CommandKey::Cancel | CommandKey::Submit) {
                self.command_open = false;
            }
            self.out.push(Event::CommandKey(key));
            return;
        }

        if let Some(key) = translate(&raw, mods) {
            self.out.push(Event::Key(key));
        }
    }

    /// Timeline column under a window point, if the point is in the timeline.
    #[must_use]
    pub fn column_at(&self, x: i32, y: i32) -> Option<u32> {
        let l = self.layout();
        if !l.tracks.contains(x, y) && !l.ruler.contains(x, y) {
            return None;
        }
        u32::try_from(x - l.tracks.x).ok()
    }

    /// Which track lane a y coordinate is over, or `None` on the ruler.
    #[must_use]
    pub fn lane_at(&self, y: i32) -> Option<usize> {
        let l = self.layout();
        if y < l.tracks.y || y >= l.tracks.y.saturating_add(l.tracks.height as i32) {
            return None;
        }
        let row = (y - l.tracks.y) / l.metrics.row_height.max(1) as i32;
        usize::try_from(row).ok()
    }

    #[must_use]
    pub fn wants_quit(&self) -> bool {
        self.quit
    }
}

impl Frontend for Gui {
    fn poll(&mut self) -> Vec<Event> {
        for event in std::mem::take(&mut self.pending) {
            self.handle(event);
        }
        std::mem::take(&mut self.out)
    }

    fn surface(&self) -> Surface {
        self.layout().surface()
    }

    fn render(&mut self, view: &ViewState) -> Result<(), AppError> {
        // A view whose command line is open is the app telling us COMMAND
        // mode is live; keep the modal in step with it rather than guessing.
        self.command_open = view.command_line.is_some();
        self.completions_shown = view
            .command_line
            .as_ref()
            .is_some_and(|c| !c.completions.is_empty());
        let layout = self.layout();
        // The picker is the shell's own state, not the app's, so it is
        // folded into the chrome here rather than carried in the view.
        let mut chrome = self.chrome.clone();
        chrome.picker = self.picker.as_ref().map(picker_view);
        self.last_draw = Some(paint(view, &layout, &chrome));
        Ok(())
    }
}

/// What the painter needs to draw the picker.
fn picker_view(picker: &MediaPicker) -> PickerView {
    let visible = picker.visible();
    PickerView {
        title: match picker.intent() {
            PickerIntent::Insert => "insert media at the playhead",
            PickerIntent::Append => "append media after this clip",
            PickerIntent::Replace => "replace this clip with",
        }
        .to_string(),
        query: picker.query().to_string(),
        entries: visible
            .iter()
            .map(|e| PickerRow {
                label: e.label.clone(),
                is_dir: e.is_dir,
            })
            .collect(),
        selected: picker.selected(),
    }
}

/// The app's intent, in the picker's vocabulary. Two enums rather than one
/// because `davimci-keys` may not depend on a frontend.
fn intent_of(intent: MediaIntent) -> PickerIntent {
    match intent {
        MediaIntent::Insert => PickerIntent::Insert,
        MediaIntent::Append => PickerIntent::Append,
        MediaIntent::Replace => PickerIntent::Replace,
    }
}

/// The picker's rows for a directory, from the app's shared listing so the
/// GUI and the TUI show the same files in the same order.
fn entries_for(dir: &std::path::Path) -> Vec<Entry> {
    davimci_app::list_dir(dir)
        .into_iter()
        .map(|e| Entry {
            path: e.path.to_string_lossy().to_string(),
            // Keep the browser's label: deriving it from the path would
            // turn `..` into the parent directory's name.
            label: e.label,
            is_dir: e.is_dir,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use davimci_keys::Key;

    fn gui() -> Gui {
        Gui::new(800, 600)
    }

    #[test]
    fn keys_become_app_key_events() {
        let mut g = gui();
        g.push(GuiEvent::Key(RawKey::Char('d'), Modifiers::default()));
        g.push(GuiEvent::Key(RawKey::Char('w'), Modifiers::default()));
        assert_eq!(
            g.poll(),
            vec![Event::Key(Key::Char('d')), Event::Key(Key::Char('w'))]
        );
    }

    #[test]
    fn resizing_reports_a_new_surface() {
        let mut g = gui();
        g.push(GuiEvent::Resized {
            width: 400,
            height: 300,
        });
        let events = g.poll();
        assert!(matches!(events.as_slice(), [Event::Resize(_)]));
        assert_eq!(g.surface().columns, 400 - 80);
    }

    #[test]
    fn the_command_line_owns_the_keyboard_while_it_is_open() {
        let mut g = gui();
        g.open_command_line();
        for c in "wq".chars() {
            g.push(GuiEvent::Key(RawKey::Char(c), Modifiers::default()));
        }
        // The buffer lives in the app, so the keys go there as command
        // keys rather than into the grammar.
        assert_eq!(
            g.poll(),
            vec![
                Event::CommandKey(CommandKey::Char('w')),
                Event::CommandKey(CommandKey::Char('q')),
            ],
            "keys leaked into the grammar"
        );
        g.push(GuiEvent::Key(RawKey::Enter, Modifiers::default()));
        assert_eq!(g.poll(), vec![Event::CommandKey(CommandKey::Submit)]);
        // And gives it back afterwards.
        g.push(GuiEvent::Key(RawKey::Char('x'), Modifiers::default()));
        assert_eq!(g.poll(), vec![Event::Key(Key::Char('x'))]);
    }

    #[test]
    fn escaping_the_command_line_cancels_it() {
        let mut g = gui();
        g.open_command_line();
        g.push(GuiEvent::Key(RawKey::Escape, Modifiers::default()));
        assert_eq!(g.poll(), vec![Event::CommandKey(CommandKey::Cancel)]);
    }

    #[test]
    fn the_picker_swallows_keys_until_it_closes() {
        let mut g = gui();
        g.open_picker(MediaPicker::new(
            crate::picker::PickerIntent::Insert,
            vec![crate::picker::Entry::file("/m/a.mkv")],
        ));
        g.push(GuiEvent::Key(RawKey::Char('a'), Modifiers::default()));
        assert!(g.poll().is_empty());
        assert_eq!(g.picker().map(|p| p.query().to_string()), Some("a".into()));
        g.push(GuiEvent::Key(RawKey::Enter, Modifiers::default()));
        g.poll();
        assert!(g.picker().is_none());
    }

    #[test]
    fn subtitle_insert_mode_takes_text_and_escapes_back() {
        let mut g = gui();
        g.open_subtitle(SubtitleEdit::new(davimci_core::ClipId(1), ""));
        g.push(GuiEvent::Key(RawKey::Char('h'), Modifiers::default()));
        assert!(g.poll().is_empty());
        assert_eq!(
            g.subtitle().map(|s| s.buffer().to_string()),
            Some("h".into())
        );
        g.push(GuiEvent::Key(RawKey::Escape, Modifiers::default()));
        g.poll();
        assert!(g.subtitle().is_none());
    }

    #[test]
    fn closing_the_window_quits() {
        let mut g = gui();
        g.push(GuiEvent::CloseRequested);
        assert_eq!(g.poll(), vec![Event::Quit]);
        assert!(g.wants_quit());
    }

    #[test]
    fn a_click_outside_the_timeline_is_not_a_column() {
        let g = gui();
        assert_eq!(g.column_at(10, 0), None);
        let l = g.layout();
        assert_eq!(g.column_at(l.tracks.x + 5, l.tracks.y + 1), Some(5));
    }

    /// A click reaches the app as a position, and the ruler is not a lane.
    #[test]
    fn a_click_becomes_a_seek_event_with_its_lane() {
        let mut g = gui();
        let l = g.layout();
        g.push(GuiEvent::Click {
            x: l.tracks.x + 7,
            y: l.tracks.y + l.metrics.row_height as i32 + 1,
        });
        assert_eq!(
            g.poll(),
            vec![Event::Click {
                column: 7,
                row: Some(1)
            }]
        );

        g.push(GuiEvent::Click {
            x: l.tracks.x + 3,
            y: l.ruler.y,
        });
        assert_eq!(
            g.poll(),
            vec![Event::Click {
                column: 3,
                row: None
            }],
            "a click on the ruler seeks without changing track focus"
        );
    }
}
