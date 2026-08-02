//! The [`Frontend`] trait and the event loop that is generic over it
//! (plan.md Phase 9a).
//!
//! A frontend does three things: report events, report its size, and draw a
//! [`ViewState`]. It contains no view logic, so anything a GUI and a TUI would
//! both have to implement belongs above this line, not below it.

use davimci_keys::Key;

use crate::error::AppError;
use crate::view::ViewState;

/// Size of the timeline area, in whatever unit the frontend draws in - GUI
/// pixels or TUI cells. The app does not care which, only that it is
/// consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Surface {
    pub columns: u32,
    pub rows: usize,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            columns: 80,
            rows: 4,
        }
    }
}

/// Something that happened outside the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Key(Key),
    Resize(Surface),
    /// A `:` line was submitted. The frontend owns command-line editing (it
    /// has the text widget); the app owns what the line means.
    Command(String),
    /// The `:` line was abandoned.
    CommandCancelled,
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
