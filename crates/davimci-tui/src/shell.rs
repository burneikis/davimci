//! The terminal frontend's state machine.
//!
//! [`Tui`] is a [`Frontend`] like the GUI: it turns terminal events into
//! `davimci-app` events and draws the [`ViewState`] it is handed. Modal input
//! is routed by [`davimci_app::Modals`], the same router the window uses, so
//! the `:` line, the picker and subtitle editing behave identically in both.
//!
//! It holds no terminal. [`crate::terminal`] feeds it events and rasterises
//! the rows it produced, which is what lets every test here run with no tty.

use davimci_app::{
    AppError, Event, Frontend, MediaPicker, ModalKey, Modals, PickerIntent, Response, SubtitleEdit,
    Surface, ViewState,
};
use ratatui::prelude::Line;

use crate::input::{Modifiers, TermKey, translate};
use crate::render::{self, Overlay};

/// Something the terminal observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    Key(TermKey, Modifiers),
    Resize {
        width: u16,
        height: u16,
    },
    /// A click in the terminal, in cells.
    Click {
        column: u16,
        row: u16,
    },
    /// The clock ticked: repaint, poll jobs, pull a preview frame.
    Tick,
    /// The terminal went away (`SIGHUP`, closed emulator).
    Closed,
}

/// The terminal frontend.
#[derive(Debug)]
pub struct Tui {
    width: u16,
    height: u16,
    pending: Vec<TermEvent>,
    out: Vec<Event>,
    modals: Modals,
    /// Rows the `:` line took at the last render, so the surface reported
    /// between renders matches what is actually on screen.
    command_rows: u16,
    last_lines: Vec<Line<'static>>,
    quit: bool,
}

impl Tui {
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            pending: Vec::new(),
            out: Vec::new(),
            modals: Modals::new(),
            command_rows: 0,
            last_lines: Vec::new(),
            quit: false,
        }
    }

    pub fn push(&mut self, event: TermEvent) {
        self.pending.push(event);
    }

    /// React to what the app decided - open a picker, open the `:` line.
    pub fn apply_response(&mut self, response: &Response) {
        self.modals.apply_response(response);
    }

    #[must_use]
    pub fn picker(&self) -> Option<&MediaPicker> {
        self.modals.picker()
    }

    pub fn open_picker(&mut self, picker: MediaPicker) {
        self.modals.open_picker(picker);
    }

    /// Open the picker on `dir` for `intent`.
    pub fn open_picker_at(&mut self, intent: PickerIntent, dir: &std::path::Path) {
        self.modals.open_picker_at(intent, dir);
    }

    #[must_use]
    pub fn subtitle(&self) -> Option<&SubtitleEdit> {
        self.modals.subtitle()
    }

    pub fn open_subtitle(&mut self, edit: SubtitleEdit) {
        self.modals.open_subtitle(edit);
    }

    #[must_use]
    pub fn command_is_open(&self) -> bool {
        self.modals.command_is_open()
    }

    #[must_use]
    pub fn wants_quit(&self) -> bool {
        self.quit
    }

    /// The rows drawn at the last render - what [`crate::terminal`]
    /// rasterises.
    #[must_use]
    pub fn last_lines(&self) -> &[Line<'static>] {
        &self.last_lines
    }

    /// The same rows as plain text, which is what the snapshot tests read.
    #[must_use]
    pub fn last_rows(&self) -> Vec<String> {
        render::plain(&self.last_lines)
    }

    /// The rows a view would produce at the current size, styles included.
    #[must_use]
    pub fn rows(&self, view: &ViewState) -> Vec<Line<'static>> {
        render::lines(
            view,
            Overlay {
                picker: self.modals.picker(),
                subtitle: self.modals.subtitle(),
            },
            self.width,
            self.height,
        )
    }

    fn handle(&mut self, event: TermEvent) {
        match event {
            TermEvent::Resize { width, height } => {
                self.width = width;
                self.height = height;
                self.out.push(Event::Resize(self.surface()));
            }
            TermEvent::Tick => self.out.push(Event::Tick),
            TermEvent::Closed => {
                self.quit = true;
                self.out.push(Event::Quit);
            }
            TermEvent::Click { column, row } => {
                // Where, never what it means: the app owns navigation.
                if column >= render::GUTTER {
                    self.out.push(Event::Click {
                        column: u32::from(column - render::GUTTER),
                        // Row 0 is the ruler, which seeks without changing
                        // which track is focused.
                        row: (row > 0).then(|| usize::from(row - 1)),
                    });
                }
            }
            TermEvent::Key(key, mods) => self.handle_key(key, mods),
        }
    }

    fn handle_key(&mut self, key: TermKey, mods: Modifiers) {
        // A modal owns the keyboard while it is open. Chorded keys are never
        // a modal's, so Ctrl-o still reaches the grammar from a picker.
        if self.modals.is_open()
            && !mods.ctrl
            && !mods.alt
            && let Some(modal) = modal_key(&key)
            && let Some(events) = self.modals.handle(modal)
        {
            self.out.extend(events);
            return;
        }

        if let Some(key) = translate(&key, mods) {
            self.out.push(Event::Key(key));
        }
    }
}

impl Frontend for Tui {
    fn poll(&mut self) -> Vec<Event> {
        for event in std::mem::take(&mut self.pending) {
            self.handle(event);
        }
        std::mem::take(&mut self.out)
    }

    fn surface(&self) -> Surface {
        render::surface(self.width, self.height, self.command_rows)
    }

    fn render(&mut self, view: &ViewState) -> Result<(), AppError> {
        // A view whose command line is open is the app telling us COMMAND
        // mode is live; keep the modal in step with it rather than guessing.
        self.modals.sync(view);
        self.command_rows = render::command_rows(view);
        self.last_lines = self.rows(view);
        Ok(())
    }
}

/// One terminal key press in the modal alphabet. `None` for keys no modal can
/// use, which then fall through to the grammar.
fn modal_key(key: &TermKey) -> Option<ModalKey> {
    Some(match key {
        TermKey::Char(c) => ModalKey::Char(*c),
        TermKey::Escape => ModalKey::Escape,
        TermKey::Enter => ModalKey::Enter,
        TermKey::Backspace => ModalKey::Backspace,
        TermKey::Tab => ModalKey::Tab,
        TermKey::Left => ModalKey::Left,
        TermKey::Right => ModalKey::Right,
        TermKey::Up => ModalKey::Up,
        TermKey::Down => ModalKey::Down,
        TermKey::Other => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use davimci_app::{CommandKey, Entry};
    use davimci_keys::Key;

    fn tui() -> Tui {
        Tui::new(80, 12)
    }

    fn press(tui: &mut Tui, c: char) {
        tui.push(TermEvent::Key(TermKey::Char(c), Modifiers::default()));
    }

    #[test]
    fn keys_become_app_key_events() {
        let mut t = tui();
        press(&mut t, 'd');
        press(&mut t, 'w');
        assert_eq!(
            t.poll(),
            vec![Event::Key(Key::Char('d')), Event::Key(Key::Char('w'))]
        );
    }

    #[test]
    fn resizing_reports_a_new_surface() {
        let mut t = tui();
        t.push(TermEvent::Resize {
            width: 40,
            height: 8,
        });
        let events = t.poll();
        assert!(matches!(events.as_slice(), [Event::Resize(_)]));
        assert_eq!(t.surface().columns, 40 - u32::from(render::GUTTER));
        assert_eq!(t.surface().rows, 6);
    }

    #[test]
    fn the_command_line_owns_the_keyboard_while_it_is_open() {
        let mut t = tui();
        t.modals.open_command_line();
        press(&mut t, 'w');
        press(&mut t, 'q');
        assert_eq!(
            t.poll(),
            vec![
                Event::CommandKey(CommandKey::Char('w')),
                Event::CommandKey(CommandKey::Char('q')),
            ],
            "keys leaked into the grammar"
        );
        t.push(TermEvent::Key(TermKey::Enter, Modifiers::default()));
        assert_eq!(t.poll(), vec![Event::CommandKey(CommandKey::Submit)]);
        press(&mut t, 'x');
        assert_eq!(t.poll(), vec![Event::Key(Key::Char('x'))]);
    }

    #[test]
    fn the_picker_swallows_keys_until_it_closes() {
        let mut t = tui();
        t.open_picker(MediaPicker::new(
            PickerIntent::Insert,
            vec![Entry::file("/m/a.mkv")],
        ));
        press(&mut t, 'a');
        assert!(t.poll().is_empty());
        assert_eq!(t.picker().map(|p| p.query().to_string()), Some("a".into()));
        t.push(TermEvent::Key(TermKey::Enter, Modifiers::default()));
        t.poll();
        assert!(t.picker().is_none());
    }

    #[test]
    fn a_click_reports_its_column_and_lane() {
        let mut t = tui();
        t.push(TermEvent::Click {
            column: render::GUTTER + 7,
            row: 2,
        });
        assert_eq!(
            t.poll(),
            vec![Event::Click {
                column: 7,
                row: Some(1)
            }]
        );
        t.push(TermEvent::Click {
            column: render::GUTTER + 3,
            row: 0,
        });
        assert_eq!(
            t.poll(),
            vec![Event::Click {
                column: 3,
                row: None
            }],
            "a click on the ruler seeks without changing track focus"
        );
        // The gutter is not the timeline.
        t.push(TermEvent::Click { column: 1, row: 2 });
        assert!(t.poll().is_empty());
    }

    #[test]
    fn losing_the_terminal_quits() {
        let mut t = tui();
        t.push(TermEvent::Closed);
        assert_eq!(t.poll(), vec![Event::Quit]);
        assert!(t.wants_quit());
    }
}
