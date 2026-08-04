//! Scriptable headless frontend.
//!
//! It is a [`Frontend`] like any other: it polls events, reports a size, and
//! draws a [`ViewState`] - except that "draws" means appending the view's
//! textual dump. That is what makes the cross-frontend parity test possible:
//! the same script through headless, GUI, and TUI must produce the same view
//! states, and any difference is a frontend bug by construction.
//!
//! [`script`] adds the file format on top: keystrokes and assertions in one
//! artefact, so a bug report and a test are the same thing.

pub mod script;

pub use script::{Failure, ParseError, Report, Script};

use std::collections::VecDeque;

use davimci_app::{AppError, Event, Frontend, Surface, ViewState};
use davimci_keys::Key;

/// A frontend that replays a fixed event script and records what it was asked
/// to draw.
#[derive(Debug)]
pub struct HeadlessFrontend {
    events: VecDeque<Event>,
    surface: Surface,
    frames: Vec<String>,
    /// Renders after the script is exhausted, so a test can watch a repaint
    /// without the loop spinning forever.
    trailing_renders: usize,
    rendered_after_script: usize,
}

impl HeadlessFrontend {
    #[must_use]
    pub fn new(surface: Surface) -> Self {
        Self {
            events: VecDeque::new(),
            surface,
            frames: Vec::new(),
            trailing_renders: 1,
            rendered_after_script: 0,
        }
    }

    /// Build from a vim-style key string, e.g. `"3lyy"`.
    #[must_use]
    pub fn script(surface: Surface, keys: &str) -> Self {
        let mut f = Self::new(surface);
        f.push_keys(keys);
        f
    }

    pub fn push_keys(&mut self, keys: &str) {
        for k in Key::parse_str(keys) {
            self.events.push_back(Event::Key(k));
        }
    }

    pub fn push_key(&mut self, key: Key) {
        self.events.push_back(Event::Key(key));
    }

    pub fn push_event(&mut self, event: Event) {
        self.events.push_back(event);
    }

    /// Number of repaints allowed after the script runs out, before the
    /// frontend quits. Default 1.
    pub fn set_trailing_renders(&mut self, n: usize) {
        self.trailing_renders = n;
    }

    /// Every view drawn so far, oldest first.
    #[must_use]
    pub fn frames(&self) -> &[String] {
        &self.frames
    }

    /// The last view drawn - the parity test's comparison point.
    #[must_use]
    pub fn last_frame(&self) -> Option<&str> {
        self.frames.last().map(String::as_str)
    }
}

impl Frontend for HeadlessFrontend {
    fn poll(&mut self) -> Vec<Event> {
        match self.events.pop_front() {
            Some(e) => vec![e],
            None if self.rendered_after_script >= self.trailing_renders => vec![Event::Quit],
            None => Vec::new(),
        }
    }

    fn surface(&self) -> Surface {
        self.surface
    }

    fn render(&mut self, view: &ViewState) -> Result<(), AppError> {
        self.frames.push(view.dump());
        if self.events.is_empty() {
            self.rendered_after_script = self.rendered_after_script.saturating_add(1);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_app::{App, NullHost};
    use davimci_cmd::Session;
    use davimci_core::testing::fixture;

    #[test]
    fn a_script_drives_the_app_to_a_final_view() {
        let session = Session::new(fixture(&[("V1", &[(0, 100, "a"), (100, 100, "b")])]));
        let mut app = App::new(session);
        let mut fe = HeadlessFrontend::script(
            Surface {
                columns: 40,
                rows: 2,
                ..Surface::default()
            },
            "lls",
        );
        let mut host = NullHost;
        app.run(&mut fe, &mut host).unwrap();
        assert!(fe.last_frame().unwrap().starts_with("-- NORMAL (V1) --"));
        assert!(!fe.frames().is_empty());
    }
}
