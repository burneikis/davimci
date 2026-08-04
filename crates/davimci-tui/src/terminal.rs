//! The terminal itself: raw mode, the alternate screen, and the event pump.
//!
//! This is the only file in the crate that does I/O. Everything it feeds
//! [`crate::Tui`] is already in the frontend's own vocabulary, and everything
//! it draws was decided by [`crate::render`], so a broken terminal can lose
//! pixels but never change what the editor did.

use std::io::{self, Stdout, Write};
use std::time::Duration;

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
use crate::shell::TermEvent;

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

    /// Draw one screen.
    pub fn draw(&mut self, lines: &[Line<'_>]) -> io::Result<()> {
        self.inner.draw(|frame| {
            frame.render_widget(Paragraph::new(lines.to_vec()), frame.area());
        })?;
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
