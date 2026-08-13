//! The window shell's state machine.
//!
//! [`Gui`] is a [`Frontend`]: it turns window events into `davimci-app`
//! events and draws the [`ViewState`] it is handed. Modal input - the `:`
//! line, the media picker, INSERT-mode subtitle editing - is routed here,
//! because a modal owns the keyboard and the key grammar must not see those
//! keystrokes.
//!
//! The windowing layer (not yet written, see the crate docs) does exactly two
//! things: push [`GuiEvent`]s in, and rasterise the [`DrawList`] out.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "pointer coordinates are window-sized, so a wrap is not reachable"
)]

use davimci_app::{AppError, Event, Frontend, Response, Surface, ViewState};
use davimci_app::{MediaPicker, Modals, PickerIntent, SubtitleEdit};

use crate::layout::{Layout, Metrics, VideoHeight, paint};
use crate::paint::{Chrome, DrawList, PickerRow, PickerView};
use davimci_app::rawkey::{Modifiers, RawKey, modal_key, translate};

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
    /// The `:` line, the picker and subtitle editing, routed by the app so
    /// the GUI and the TUI cannot disagree about who owns the keyboard.
    modals: Modals,
    /// Whether the last view had suggestions, so the layout keeps a row for
    /// them. Read from the view rather than decided here.
    completions_shown: bool,
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
            modals: Modals::new(),
            completions_shown: false,
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

    /// `:set previewheight`, in the window's terms. [`VideoHeight::Off`]
    /// gives the whole window to the timeline.
    pub fn set_preview_height(&mut self, height: VideoHeight) {
        self.metrics.video = height;
    }

    #[must_use]
    pub fn preview_height(&self) -> VideoHeight {
        self.metrics.video
    }

    #[must_use]
    pub fn layout(&self) -> Layout {
        Layout::compute(
            self.width,
            self.height,
            self.metrics,
            self.modals.command_is_open(),
            self.completions_shown,
        )
    }

    #[must_use]
    pub fn last_draw(&self) -> Option<&DrawList> {
        self.last_draw.as_ref()
    }

    #[must_use]
    pub fn command_is_open(&self) -> bool {
        self.modals.command_is_open()
    }

    /// Whether a modal is currently spelling out text, which is the only
    /// place a pasted line has anywhere to go.
    #[must_use]
    pub fn takes_text(&self) -> bool {
        self.modals.command_is_open()
            || self.modals.picker().is_some()
            || self.modals.subtitle().is_some()
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

    /// React to what the app decided. A frontend that ignored this would
    /// simply never show a picker.
    pub fn apply_response(&mut self, response: &Response) {
        self.modals.apply_response(response);
    }

    pub fn open_subtitle(&mut self, edit: SubtitleEdit) {
        self.modals.open_subtitle(edit);
    }

    #[must_use]
    pub fn subtitle(&self) -> Option<&SubtitleEdit> {
        self.modals.subtitle()
    }

    /// The app told us it entered `COMMAND` mode.
    pub fn open_command_line(&mut self) {
        self.modals.open_command_line();
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
            GuiEvent::Key(raw, mods) => self.handle_key(&raw, mods),
        }
    }

    fn handle_key(&mut self, raw: &RawKey, mods: Modifiers) {
        // A modal owns the keyboard while it is open, and which one owns it
        // is the app's decision, not the shell's. Chorded keys are never a
        // modal's, so Ctrl-o still reaches the grammar from a picker.
        if self.modals.is_open()
            && !mods.ctrl
            && !mods.alt
            && !mods.logo
            && let Some(modal) = modal_key(raw)
            && let Some(events) = self.modals.handle(modal)
        {
            self.out.extend(events);
            return;
        }

        if let Some(key) = translate(raw, mods) {
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
        self.modals.sync(view);
        self.completions_shown = view
            .command_line
            .as_ref()
            .is_some_and(|c| !c.completions.is_empty());
        let layout = self.layout();
        // The picker is the shell's own state, not the app's, so it is
        // folded into the chrome here rather than carried in the view.
        let mut chrome = self.chrome.clone();
        chrome.picker = self.modals.picker().map(picker_view);
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

#[cfg(test)]
mod tests {
    use super::*;
    use davimci_app::{CommandKey, Entry};
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

    /// A chord is the grammar's, open modal or not - the same rule the
    /// terminal follows.
    #[test]
    fn a_chord_reaches_the_grammar_through_an_open_modal() {
        let mut g = gui();
        g.open_picker(MediaPicker::new(
            PickerIntent::Insert,
            vec![Entry::file("/m/a.mkv")],
        ));
        g.push(GuiEvent::Key(RawKey::Char('o'), Modifiers::ctrl()));
        assert_eq!(g.poll(), vec![Event::Key(Key::Ctrl('o'))]);
        assert_eq!(
            g.picker().map(|p| p.query().to_string()),
            Some(String::new())
        );
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
            PickerIntent::Insert,
            vec![Entry::file("/m/a.mkv")],
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

    /// `:set previewheight 0` is honoured by the window, not only by the
    /// terminal: the timeline takes the pane's pixels and the surface grows.
    #[test]
    fn preview_height_reaches_the_layout_and_the_surface() {
        let mut g = gui();
        let tall = g.layout().tracks.height;
        assert!(tall > 0);
        g.set_preview_height(VideoHeight::Off);
        assert_eq!(g.layout().video.height, 0);
        assert!(g.layout().tracks.height > tall);
        assert!(g.surface().rows > 0);
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
