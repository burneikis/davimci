//! The terminal itself: raw mode, the alternate screen, and the event pump.
//!
//! This is the only file in the crate that does I/O. Everything it feeds
//! [`crate::Tui`] is already in the frontend's own vocabulary, and everything
//! it draws was decided by [`crate::render`], so a broken terminal can lose
//! pixels but never change what the editor did.

use std::io::{self, Stdout, Write};
use std::time::{Duration, Instant};

use crossterm::cursor::MoveTo;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event as CtEvent, MouseButton, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use crossterm::{execute, queue};
use ratatui::backend::CrosstermBackend;
use ratatui::prelude::Line;
use ratatui::widgets::Paragraph;

use crate::input::from_crossterm;
use crate::preview::{Cell, Protocol};
use crate::shell::TermEvent;

/// How long a capability probe waits. Long enough for a local terminal and a
/// multiplexer to answer, short enough that a terminal which never will costs
/// a blink of startup rather than a hang.
const PROBE_TIMEOUT: Duration = Duration::from_millis(100);

/// Read whatever the terminal replies to a probe, up to `timeout`.
///
/// Read straight from the input fd rather than through `crossterm`, which
/// parses device-attribute replies and discards their contents, and does not
/// recognise a kitty graphics reply at all. Safe only before the event pump
/// starts, which is the only place this is called from.
fn read_reply(timeout: Duration) -> Option<String> {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let stdin = rustix::stdio::stdin();
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let spec = Timespec {
            tv_sec: i64::try_from(left.as_secs()).unwrap_or(0),
            tv_nsec: i64::from(left.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        if poll(&mut fds, Some(&spec)).ok()? == 0 {
            return None;
        }
        let mut chunk = [0u8; 256];
        let read = rustix::io::read(stdin, &mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
        // The device-attributes reply is the terminator: it is sent last and
        // every terminal sends it, so its final `c` means the answer is whole.
        if let Some(start) = find(&buf, b"\x1b[?")
            && buf[start..].contains(&b'c')
        {
            return Some(String::from_utf8_lossy(&buf).into_owned());
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// What a probe reply says the terminal can draw, if anything.
///
/// Kitty is asked first because a terminal with both should use it: it carries
/// full colour, where sixel is a palette per frame.
fn protocol_from_reply(reply: &str) -> Option<Protocol> {
    if reply.contains("_Gi=31;OK") {
        return Some(Protocol::Kitty);
    }
    // Device attributes list capabilities as numbers, and `4` is sixel.
    let attributes = reply.split('\x1b').find(|s| s.starts_with("[?"))?;
    let sixel = attributes
        .trim_start_matches("[?")
        .trim_end_matches('c')
        .split(';')
        .any(|n| n == "4");
    sixel.then_some(Protocol::Sixel)
}

/// A terminal in raw mode on the alternate screen, restored on drop.
pub struct Terminal {
    inner: ratatui::Terminal<CrosstermBackend<Stdout>>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal").finish_non_exhaustive()
    }
}

impl Terminal {
    /// Take over the terminal.
    pub fn open() -> io::Result<Self> {
        let mut out = io::stdout();
        enable_raw_mode()?;
        // Mouse capture is what makes a click a seek; a terminal that
        // refuses it simply has no clicking, so the failure is not fatal.
        execute!(out, EnterAlternateScreen)?;
        let _ = execute!(out, EnableMouseCapture);
        let inner = ratatui::Terminal::new(CrosstermBackend::new(out))?;
        Ok(Self { inner })
    }

    /// Current size in cells.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        let area = self.inner.size()?;
        Ok((area.width, area.height))
    }

    /// What one cell measures in pixels, for the graphics protocols.
    ///
    /// A terminal that will not say falls back to [`Cell::default`] rather
    /// than refusing to preview: being wrong by a little skews a graphics
    /// preview's aspect, and the layout is counted in cells either way.
    #[must_use]
    pub fn cell(&self) -> Cell {
        let Ok(size) = crossterm::terminal::window_size() else {
            return Cell::default();
        };
        if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
            return Cell::default();
        }
        Cell {
            width: size.width / size.columns,
            height: size.height / size.rows,
        }
    }

    /// Ask the terminal what it can draw, once, before the input loop starts.
    ///
    /// The environment is not always enough: a patched build has whatever
    /// `TERM` its distribution gave it, so a terminal with sixel can look like
    /// one without. The query is a kitty graphics probe followed by a
    /// device-attributes request, because every terminal answers the latter -
    /// that reply is the terminator, so an unsupported probe costs a round
    /// trip rather than a timeout. A terminal that says nothing in
    /// `PROBE_TIMEOUT` gets [`None`], and the caller keeps whatever the
    /// environment suggested.
    ///
    /// Only ever called here, at startup: a query per frame would be both slow
    /// and, through a multiplexer, wrong, which is why
    /// `:set previewprotocol` overrides all of this.
    pub fn query_graphics(&mut self) -> Option<Protocol> {
        let mut out = io::stdout();
        // A 1x1 RGB image by direct transmission: kitty replies `_Gi=31;OK`,
        // and a terminal without the protocol ignores it.
        out.write_all(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c")
            .ok()?;
        out.flush().ok()?;
        protocol_from_reply(&read_reply(PROBE_TIMEOUT)?)
    }

    /// Events waiting, oldest first, blocking at most `timeout`.
    ///
    /// Everything the frontend does not bind - focus changes, paste, a mouse
    /// move - is dropped here rather than translated into a key nobody asked
    /// for.
    pub fn poll(&mut self, timeout: Duration) -> io::Result<Vec<TermEvent>> {
        let mut out = Vec::new();
        if !crossterm::event::poll(timeout)? {
            return Ok(out);
        }
        // Drain the queue: a held key delivers a burst, and the app batches
        // it into one seek.
        while crossterm::event::poll(Duration::ZERO)? {
            match crossterm::event::read()? {
                CtEvent::Key(key) => {
                    if let Some((key, mods)) = from_crossterm(&key) {
                        out.push(TermEvent::Key(key, mods));
                    }
                }
                CtEvent::Resize(width, height) => out.push(TermEvent::Resize { width, height }),
                CtEvent::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left) => {
                    out.push(TermEvent::Click {
                        column: m.column,
                        row: m.row,
                    });
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Draw one screen, plus whatever a graphics preview wants written over
    /// the band it was given.
    ///
    /// The escape goes out after the rows, at the home position, because the
    /// rows the picture covers were drawn blank on purpose: `ratatui` knows
    /// nothing about an image and would otherwise overwrite it on the next
    /// diff.
    pub fn draw(
        &mut self,
        lines: &[Line<'_>],
        preview: Option<&[u8]>,
        cursor: Option<(u16, u16)>,
    ) -> io::Result<()> {
        self.inner.draw(|frame| {
            frame.render_widget(Paragraph::new(lines.to_vec()), frame.area());
            // Shown only while something is being typed: a block caret parked
            // on the timeline would read as a second playhead.
            if let Some((column, row)) = cursor {
                frame.set_cursor_position((column, row));
            }
        })?;
        if let Some(bytes) = preview {
            let mut out = io::stdout();
            queue!(out, MoveTo(0, 0))?;
            out.write_all(bytes)?;
            out.flush()?;
        }
        Ok(())
    }

    /// Give the terminal back. Called by [`Drop`] too, so a panic on the way
    /// out still leaves a usable shell.
    pub fn close(&mut self) {
        let mut out = io::stdout();
        let _ = queue!(out, DisableMouseCapture, LeaveAlternateScreen);
        let _ = out.flush();
        let _ = disable_raw_mode();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_reply_names_the_protocol_it_proves() {
        // kitty answers the graphics probe and the attributes request both.
        assert_eq!(
            protocol_from_reply("\x1b_Gi=31;OK\x1b\\\x1b[?62;c"),
            Some(Protocol::Kitty)
        );
        // A sixel terminal ignores the probe and lists 4 in its attributes.
        assert_eq!(
            protocol_from_reply("\x1b[?62;4;6;9;22c"),
            Some(Protocol::Sixel)
        );
        // Neither: the caller keeps what the environment suggested.
        assert_eq!(protocol_from_reply("\x1b[?62;22c"), None);
        // A number containing 4 is not the number 4.
        assert_eq!(protocol_from_reply("\x1b[?64;41c"), None);
        // Nothing recognisable at all, rather than a panic.
        assert_eq!(protocol_from_reply(""), None);
    }
}
