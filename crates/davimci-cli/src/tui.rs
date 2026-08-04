//! The terminal session (`--tui`).
//!
//! The counterpart to [`crate::window`]: the one place that holds the
//! terminal frontend *and* a render backend, because no frontend may
//! reference MLT (spec 10.1). It decides nothing - the rows were computed by
//! `davimci-tui` from a view state `davimci-app` assembled.
//!
//! Preview is detached here: a terminal cannot hold a picture, so the
//! presenter runs in `Detached` mode and `:set preview off` turns it off
//! entirely for sessions with no display at all.

use std::time::Duration;

use anyhow::{Context, Result};
use davimci_app::{App, Event, Frontend, Host};
use davimci_tui::{Terminal, Tui};

use crate::editor::Editor;

/// How long a quiet loop waits for input before ticking. Playback and shuttle
/// advance off the clock, so the loop cannot simply block on the keyboard.
const TICK: Duration = Duration::from_millis(16);

/// Run the editor in the terminal until it quits.
pub fn run(mut app: App, mut editor: Editor) -> Result<()> {
    let mut term = Terminal::open().context("the terminal could not be put into raw mode")?;
    let (width, height) = term.size().unwrap_or((80, 24));
    let mut tui = Tui::new(width, height);
    app.resize(tui.surface());

    loop {
        for event in term
            .poll(TICK)
            .context("the terminal stopped reporting events")?
        {
            tui.push(event);
        }
        // One poll is one batch: a held key repeats faster than a frame can
        // be decoded, so the whole burst costs a single seek.
        let events = tui.poll();
        for response in app.drain(events, &mut editor) {
            // `i`/`a`/`r` ask for a picker; the frontend is what has one.
            tui.apply_response(&response);
        }
        app.event(Event::Tick, &mut editor);

        // The editor may have swapped the timeline under us (`:e`, `:bn`).
        if let Some(session) = editor.take_session_swap() {
            app.replace_session(session);
        }
        for notice in editor.take_notices() {
            app.notify(notice);
        }

        let view = app.view();
        if let Err(e) = tui.render(&view) {
            app.notify(davimci_app::Message::error(e.to_string()));
        }
        // A failed draw loses a frame, not the session (Phase 0: recoverable
        // errors degrade locally).
        if let Err(e) = term.draw(tui.last_lines()) {
            app.notify(davimci_app::Message::error(format!(
                "the terminal could not be redrawn: {e}"
            )));
        }

        if app.wants_quit() || editor.wants_quit() || tui.wants_quit() {
            break;
        }
    }

    term.close();
    Ok(())
}
