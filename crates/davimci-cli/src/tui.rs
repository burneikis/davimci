//! The terminal session (`--tui`).
//!
//! The counterpart to [`crate::window`]: the one place that holds the
//! terminal frontend *and* a render backend, because no frontend may
//! reference MLT. It decides nothing - the rows were computed by
//! `davimci-tui` from a view state `davimci-app` assembled.
//!
//! The presenter runs in `Detached` mode, which only means overlays are
//! refused: the composited picture it produces is downsampled into the
//! terminal's own preview band. `:set preview off` stops the pulls
//! altogether for a session with no display, and `:set previewheight 0` keeps
//! them while drawing nothing.

use std::time::Duration;

use anyhow::{Context, Result};
use davimci_app::{App, Event, Frontend, Host};
use davimci_tui::{Height, Protocol, Terminal, Tui};

use crate::editor::Editor;
use crate::setting::{PreviewHeight, PreviewProtocol};

/// How long a quiet loop waits for input before ticking. Playback and shuttle
/// advance off the clock, so the loop cannot simply block on the keyboard.
const TICK: Duration = Duration::from_millis(16);

/// `:set previewprotocol`, with `auto` deferring to what startup detected.
fn resolve(setting: PreviewProtocol, detected: Protocol) -> Protocol {
    match setting {
        PreviewProtocol::Auto => detected,
        PreviewProtocol::Kitty => Protocol::Kitty,
        PreviewProtocol::Sixel => Protocol::Sixel,
        PreviewProtocol::Blocks => Protocol::Blocks,
    }
}

/// `:set previewheight`. The registry validates the value; the terminal is
/// what turns it into rows, since only it knows the screen and the picture.
fn band_height(setting: PreviewHeight) -> Height {
    match setting {
        PreviewHeight::Off => Height::Off,
        PreviewHeight::Rows(rows) => Height::Rows(rows),
        PreviewHeight::Percent(pc) => Height::Percent(pc),
        PreviewHeight::Auto => Height::Auto,
    }
}

/// Run the editor in the terminal until it quits.
pub fn run(mut app: App, mut editor: Editor) -> Result<()> {
    let mut term = Terminal::open().context("the terminal could not be put into raw mode")?;
    let (width, height) = term.size().unwrap_or((80, 24));
    let mut tui = Tui::new(width, height);
    // Detected once, here, before the event pump starts: the environment
    // first, and a probe only if it settled nothing, since a patched build
    // can have graphics its `TERM` never mentions. A query per frame would be
    // both slow and, through a multiplexer, wrong - which is why the override
    // exists.
    let mut detected = davimci_tui::detect();
    if detected == Protocol::Blocks
        && let Some(probed) = term.query_graphics()
    {
        detected = probed;
    }
    let cell = term.cell();
    tui.set_protocol(detected, cell);
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

        // View settings, read every loop: `:set previewheight` must take
        // effect on the next frame, and the surface shrinks with the band.
        tui.set_protocol(resolve(editor.preview_protocol(), detected), cell);
        let before = tui.surface();
        tui.set_preview_height(band_height(editor.preview_height()));
        if tui.surface() != before {
            app.resize(tui.surface());
        }
        tui.present(editor.presentation());

        let view = app.view();
        if let Err(e) = tui.render(&view) {
            app.notify(davimci_app::Message::error(e.to_string()));
        }
        // A failed draw loses a frame, not the session (Phase 0: recoverable
        // errors degrade locally).
        if let Err(e) = term.draw(tui.last_lines(), tui.preview_escape()) {
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
