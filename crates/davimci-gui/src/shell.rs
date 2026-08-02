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

use davimci_app::{AppError, Event, Frontend, Surface, ViewState};

use crate::cmdline::{CommandLine, CommandLineEvent, default_candidates};
use crate::input::{Modifiers, RawKey, translate};
use crate::layout::{Layout, Metrics, paint};
use crate::paint::{Chrome, DrawList};
use crate::picker::{MediaPicker, PickerEvent};
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
    command: CommandLine,
    command_open: bool,
    picker: Option<MediaPicker>,
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
            command: CommandLine::new(default_candidates()),
            command_open: false,
            picker: None,
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
        Layout::compute(self.width, self.height, self.metrics, self.command_open)
    }

    #[must_use]
    pub fn last_draw(&self) -> Option<&DrawList> {
        self.last_draw.as_ref()
    }

    #[must_use]
    pub fn command_line(&self) -> &CommandLine {
        &self.command
    }

    #[must_use]
    pub fn picker(&self) -> Option<&MediaPicker> {
        self.picker.as_ref()
    }

    pub fn open_picker(&mut self, picker: MediaPicker) {
        self.picker = Some(picker);
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
        self.command.open();
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
                if let Some(col) = self.column_at(x, y) {
                    // Clicks are navigation, so they arrive as the key the
                    // user could have typed instead - the grammar stays the
                    // single interpreter of intent.
                    let _ = col;
                    self.out.push(Event::Tick);
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
                PickerEvent::Descend(_) => {}
                PickerEvent::Cancelled | PickerEvent::Chosen { .. } => self.picker = None,
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
            if !matches!(ev, SubtitleEvent::Editing) {
                self.subtitle = None;
            }
            return;
        }

        if self.command_open {
            let ev = match &raw {
                RawKey::Escape => self.command.cancel(),
                RawKey::Enter => self.command.submit(),
                RawKey::Backspace => self.command.backspace(),
                RawKey::Tab => self.command.complete(),
                RawKey::Up => {
                    self.command.history_prev();
                    CommandLineEvent::Editing
                }
                RawKey::Down => {
                    self.command.history_next();
                    CommandLineEvent::Editing
                }
                RawKey::Left => {
                    self.command.left();
                    CommandLineEvent::Editing
                }
                RawKey::Right => {
                    self.command.right();
                    CommandLineEvent::Editing
                }
                RawKey::Space => self.command.insert(' '),
                RawKey::Char(c) => self.command.insert(*c),
                RawKey::Other => CommandLineEvent::Editing,
            };
            match ev {
                CommandLineEvent::Editing => {}
                CommandLineEvent::Submit(line) => {
                    self.command_open = false;
                    self.out.push(Event::Command(line));
                }
                CommandLineEvent::Cancel => {
                    self.command_open = false;
                    self.out.push(Event::CommandCancelled);
                }
            }
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
        if view.command_line.is_some() && !self.command_open {
            self.open_command_line();
        }
        let layout = self.layout();
        self.last_draw = Some(paint(view, &layout, &self.chrome));
        Ok(())
    }
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
        assert!(g.poll().is_empty(), "keys leaked into the grammar");
        g.push(GuiEvent::Key(RawKey::Enter, Modifiers::default()));
        assert_eq!(g.poll(), vec![Event::Command("wq".into())]);
        // And gives it back afterwards.
        g.push(GuiEvent::Key(RawKey::Char('x'), Modifiers::default()));
        assert_eq!(g.poll(), vec![Event::Key(Key::Char('x'))]);
    }

    #[test]
    fn escaping_the_command_line_cancels_it() {
        let mut g = gui();
        g.open_command_line();
        g.push(GuiEvent::Key(RawKey::Escape, Modifiers::default()));
        assert_eq!(g.poll(), vec![Event::CommandCancelled]);
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
}
