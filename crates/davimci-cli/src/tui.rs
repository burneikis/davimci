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

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use davimci_app::{App, Event, Frontend, Host};
use davimci_tui::{Height, Protocol, Terminal, Tui};

use crate::editor::Editor;
use crate::setting::{PreviewHeight, PreviewProtocol};

/// How often the loop comes round. Playback and shuttle advance off the
/// clock, so the loop cannot simply block on the keyboard.
///
/// It is a period rather than a delay, and the difference is the whole point:
/// waiting `TICK` *and then* drawing means a loop that comes round every
/// `TICK` plus a sixel encode plus a terminal write, which at 60fps is slower
/// than the source and drops frames no smaller decode can win back.
const TICK: Duration = Duration::from_millis(16);

/// How long to wait for input so the next tick lands on `deadline`, and where
/// the following one belongs.
///
/// A loop that fell behind does not try to make up the ticks it missed: the
/// deadline is moved to the next whole period from now, so a slow terminal
/// runs at a lower steady rate rather than spinning without ever waiting.
fn pace(now: Instant, deadline: Instant, period: Duration) -> (Duration, Instant) {
    if now < deadline {
        (deadline - now, deadline + period)
    } else {
        (Duration::ZERO, now + period)
    }
}

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
/// Unset means no band: a terminal session that never asked for a picture
/// must not spend decode on one.
fn band_height(setting: Option<PreviewHeight>) -> Height {
    match setting.unwrap_or(PreviewHeight::Off) {
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

    let mut deadline = Instant::now() + TICK;
    loop {
        let (wait, next) = pace(Instant::now(), deadline, TICK);
        deadline = next;
        for event in term
            .poll(wait)
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
        tui.set_numbers(editor.numbers());
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
        // Rendering is where the frontend finds out how many rows the `:`
        // line and its completions want, so the timeline is told about the
        // smaller surface here rather than a frame late.
        let after = tui.surface();
        if after != before {
            app.resize(after);
        }
        // A failed draw loses a frame, not the session (recoverable
        // errors degrade locally).
        if let Err(e) = term.draw(tui.last_lines(), tui.preview_escape(), tui.cursor()) {
            app.notify(davimci_app::Message::error(format!(
                "the terminal could not be redrawn: {e}"
            )));
        }

        if app.wants_quit() || editor.wants_quit() || tui.wants_quit() {
            break;
        }
    }

    // Before the terminal is restored, not after: cancelling here is what
    // keeps the shell prompt from coming back to a process still encoding.
    editor.shutdown();
    term.close();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the loop waited a whole tick *and then* did its work, so
    /// its real period was a tick plus a sixel encode. At 60fps that is
    /// slower than the source, and every tick took several frames off the
    /// queue and threw all but one away.
    #[test]
    fn a_tick_is_a_deadline_rather_than_a_delay() {
        let start = Instant::now();
        let (wait, next) = pace(start, start + TICK, TICK);
        assert_eq!(wait, TICK);
        // Work took 10ms of the period; the next wait is what is left of it,
        // not another whole tick.
        let (wait, _) = pace(start + TICK + Duration::from_millis(10), next, TICK);
        assert_eq!(wait, Duration::from_millis(6));
    }

    #[test]
    fn a_loop_that_fell_behind_still_waits_for_input() {
        let start = Instant::now();
        let late = start + Duration::from_millis(100);
        let (wait, next) = pace(late, start + TICK, TICK);
        assert_eq!(wait, Duration::ZERO, "a missed deadline must not block");
        assert_eq!(
            next,
            late + TICK,
            "missed ticks are dropped rather than made up"
        );
    }
}
