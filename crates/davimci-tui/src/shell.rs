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
use davimci_core::Resolution;
use ratatui::prelude::Line;

use crate::input::{Modifiers, TermKey, translate};
use crate::preview::{Band, Cell, Encoder, Height, Layout, Protocol, natural_rows};
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
    /// `:set previewheight`, before the cap a small terminal imposes.
    preview_height: Height,
    protocol: Protocol,
    cell: Cell,
    /// The shape of the last picture offered, so `previewheight auto` and the
    /// letterbox agree on how many rows the frame can fill. A session that has
    /// not composed anything yet assumes 16:9 rather than no band at all.
    aspect: Resolution,
    encoder: Encoder,
    /// The last band encoded, held until a newer one is ready: preview is
    /// allowed to stutter, so a tick with nothing new redraws what is up.
    band: Band,
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
            preview_height: Height::Off,
            protocol: Protocol::Blocks,
            cell: Cell::default(),
            aspect: Resolution {
                width: 1920,
                height: 1080,
            },
            encoder: Encoder::new(),
            band: Band::default(),
        }
    }

    /// Which protocol the preview uses, and what a cell measures. Settled
    /// once at startup - detection or `:set previewprotocol` - never guessed
    /// per frame.
    pub fn set_protocol(&mut self, protocol: Protocol, cell: Cell) {
        if (protocol, cell) != (self.protocol, self.cell) {
            self.protocol = protocol;
            self.cell = cell;
            self.band = Band::default();
        }
    }

    #[must_use]
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// `:set previewheight`. [`Height::Off`] turns the band off.
    pub fn set_preview_height(&mut self, height: Height) {
        if height != self.preview_height {
            self.preview_height = height;
            self.band = Band::default();
        }
    }

    /// Rows the band actually occupies at this terminal size.
    ///
    /// Resolved every time it is asked rather than cached, because a resize
    /// changes it for a percentage and for `auto`, and the answer must not lag
    /// a frame behind the screen it describes.
    #[must_use]
    pub fn preview_rows(&self) -> u16 {
        self.preview_height.rows(self.height, self.natural_rows())
    }

    /// Rows the picture could fill at this width - what `auto` asks for.
    #[must_use]
    pub fn natural_rows(&self) -> u16 {
        natural_rows(self.width, self.protocol, self.cell, self.aspect)
    }

    /// The band layout the current size and settings give.
    #[must_use]
    pub fn preview_layout(&self) -> Layout {
        Layout {
            columns: self.width,
            rows: self.preview_rows(),
            protocol: self.protocol,
            cell: self.cell,
        }
    }

    /// Offer the newest composited frame to the preview.
    ///
    /// Encoding happens on another thread, so this returns immediately and a
    /// frame that cannot be encoded before the next one arrives is dropped
    /// rather than queued: the audio clock stays master and the event loop is
    /// never blocked by escape-sequence throughput.
    pub fn present(&mut self, presentation: Option<&davimci_present::Presentation>) {
        if self.preview_rows() == 0 {
            self.band = Band::default();
            return;
        }
        if let Some(p) = presentation {
            if p.surface.width > 0 && p.surface.height > 0 {
                self.aspect = p.surface;
            }
            self.encoder.submit(p, self.preview_layout());
        }
        if let Some(band) = self.encoder.take() {
            self.band = band;
        }
    }

    /// The bytes a graphics protocol wants written over the band, if any.
    #[must_use]
    pub fn preview_escape(&self) -> Option<&[u8]> {
        (self.band.rows == self.preview_rows())
            .then_some(self.band.escape.as_deref())
            .flatten()
    }

    /// The band as it is drawn: the rows `:set previewheight` asked for are
    /// reserved whether or not a frame has been encoded yet, so the timeline
    /// never moves under the user while the first picture is on its way.
    fn band(&self) -> Band {
        let rows = self.preview_rows();
        if self.band.rows == rows {
            return self.band.clone();
        }
        Band {
            rows,
            ..Band::default()
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
            &self.band(),
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
                let band = self.preview_rows();
                if column >= render::GUTTER && row >= band {
                    let row = row - band;
                    self.out.push(Event::Click {
                        column: u32::from(column - render::GUTTER),
                        // The band's first row after it is the ruler, which
                        // seeks without changing which track is focused.
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
        render::surface(
            self.width,
            self.height,
            self.command_rows,
            self.preview_rows(),
        )
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
    fn a_preview_band_takes_rows_from_the_tracks_and_shifts_the_clicks() {
        let mut t = Tui::new(80, 12);
        let full = t.surface().rows;
        t.set_preview_height(Height::Rows(3));
        assert_eq!(t.preview_rows(), 3);
        assert_eq!(t.surface().rows, full - 3);
        // Row 3 is now the ruler, so it seeks without taking a track.
        t.push(TermEvent::Click {
            column: render::GUTTER + 2,
            row: 3,
        });
        assert_eq!(
            t.poll(),
            vec![Event::Click {
                column: 2,
                row: None
            }]
        );
        t.push(TermEvent::Click {
            column: render::GUTTER + 2,
            row: 5,
        });
        assert_eq!(
            t.poll(),
            vec![Event::Click {
                column: 2,
                row: Some(1)
            }]
        );
        // A click in the picture is not a click in the timeline.
        t.push(TermEvent::Click {
            column: render::GUTTER + 2,
            row: 1,
        });
        assert!(t.poll().is_empty());
    }

    #[test]
    fn a_band_taller_than_the_screen_cap_is_capped() {
        let mut t = Tui::new(80, 12);
        t.set_preview_height(Height::Rows(30));
        assert_eq!(t.preview_rows(), 9);
    }

    /// A percentage follows the screen, and `auto` follows the width: both
    /// have to answer for the terminal they are in now, not the one they were
    /// set in.
    #[test]
    fn a_percentage_and_auto_band_follow_the_terminal() {
        let mut t = Tui::new(80, 20);
        t.set_preview_height(Height::Percent(25));
        assert_eq!(t.preview_rows(), 5);
        t.push(TermEvent::Resize {
            width: 80,
            height: 40,
        });
        let _ = t.poll();
        assert_eq!(t.preview_rows(), 10);

        // Half-blocks are one column by two pixel rows, so a 16:9 picture
        // across 80 columns is 45 pixel rows, which is 23 character rows -
        // under the 30-row cap on a 40-row screen.
        t.set_preview_height(Height::Auto);
        assert_eq!(t.natural_rows(), 23);
        assert_eq!(t.preview_rows(), 23);
        // Half the width is half the picture: 40 columns of 16:9 is 22 pixel
        // rows, so 11 character rows.
        t.push(TermEvent::Resize {
            width: 40,
            height: 40,
        });
        let _ = t.poll();
        assert_eq!(t.preview_rows(), 11);
    }

    #[test]
    fn losing_the_terminal_quits() {
        let mut t = tui();
        t.push(TermEvent::Closed);
        assert_eq!(t.poll(), vec![Event::Quit]);
        assert!(t.wants_quit());
    }
}
