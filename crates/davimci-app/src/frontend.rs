//! The [`Frontend`] trait and the event loop that is generic over it
//! (plan.md Phase 9a).
//!
//! A frontend does three things: report events, report its size, and draw a
//! [`ViewState`]. It contains no view logic, so anything a GUI and a TUI would
//! both have to implement belongs above this line, not below it.

use davimci_core::ClipId;
use davimci_keys::{Key, MediaIntent};

use crate::cmdline::CommandKey;
use crate::error::AppError;
use crate::view::ViewState;

/// Size of the timeline area, in whatever unit the frontend draws in - GUI
/// pixels or TUI cells. The app does not care which, only that it is
/// consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub columns: u32,
    pub rows: usize,
    /// How many columns wide one thumbnail is drawn, which decides how often
    /// a clip is sampled for its filmstrip (spec 15.2).
    ///
    /// The frontend knows this and the app does not - it depends on lane
    /// height and the picture's aspect - but *which* frames get sampled is a
    /// view decision, so the frontend reports the width and the app picks
    /// the frames. Zero means "no thumbnails", which is how a terminal or a
    /// test opts out.
    pub thumbnail_columns: u32,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 4,
            thumbnail_columns: 0,
        }
    }
}

/// Something that happened outside the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Resize(Surface),
    /// One keystroke into the open `:` line. The frontend names the key; the
    /// app owns the buffer, the history and the completions, so the GUI and
    /// the TUI cannot show two different `:` lines.
    CommandKey(CommandKey),
    /// A `:` line was submitted whole - a script, a test, or a frontend with
    /// its own text widget. The app owns what the line means.
    Command(String),
    /// The `:` line was abandoned.
    CommandCancelled,
    /// A click landed on the timeline at this column, and on this lane when
    /// the click was inside the track area rather than on the ruler.
    ///
    /// A frontend reports *where*, never *what it means*: seeking is
    /// navigation and the app owns navigation, exactly as it owns what a key
    /// means (spec 15.2).
    Click {
        column: u32,
        row: Option<usize>,
    },
    /// A media file was chosen in the picker. The frontend owns browsing
    /// (it has the list widget); the app owns what the choice means.
    MediaChosen(std::path::PathBuf),
    /// The picker was closed without choosing anything.
    PickerCancelled,
    /// A subtitle edit was committed. The frontend owns the text buffer (it
    /// has the widget); the app owns turning the result into a command
    /// (spec 15.4).
    TextEdited {
        clip: ClipId,
        text: String,
    },
    /// The subtitle editor closed without changing anything.
    TextEditCancelled,
    /// Time passed: repaint, poll jobs, pull a preview frame.
    Tick,
    Quit,
}

/// What the app decided to do with an event, so a frontend can react (stop
/// the loop, start blinking a cursor) without inspecting app state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    Continue,
    /// The app is in `COMMAND` mode; the frontend should show and own the
    /// `:` line until it sends [`Event::Command`] or
    /// [`Event::CommandCancelled`].
    OpenCommandLine,
    /// `i`/`a`/`r`: the frontend should open the media picker and answer with
    /// [`Event::MediaChosen`] or [`Event::PickerCancelled`].
    OpenPicker(MediaIntent),
    /// `i` on a subtitle clip: the frontend should open a text buffer holding
    /// `text` and answer with [`Event::TextEdited`] or
    /// [`Event::TextEditCancelled`] (spec 15.4).
    EditText {
        clip: ClipId,
        text: String,
    },
    Quit,
}

/// A window, a terminal, or a test harness.
pub trait Frontend {
    /// Events since the last call, oldest first. Returning an empty vector is
    /// normal and must not block.
    fn poll(&mut self) -> Vec<Event>;

    /// Current drawable size.
    fn surface(&self) -> Surface;

    /// Draw one frame. Errors here are recoverable by Phase 0 policy: the app
    /// reports them and keeps running.
    fn render(&mut self, view: &ViewState) -> Result<(), AppError>;

    /// Called once before the first render.
    fn on_start(&mut self) -> Result<(), AppError> {
        Ok(())
    }

    /// Called once after the loop ends, even when it ended in an error.
    fn on_stop(&mut self) {}
}
