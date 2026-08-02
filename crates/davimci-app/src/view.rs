//! The view state every frontend renders (plan.md Phase 9a).
//!
//! Built before any frontend so no frontend can invent its own. A frontend
//! reads a [`ViewState`] and draws it; it never queries the `Timeline`, never
//! computes a column, and never decides what the status line says. That is
//! what keeps `davimci-gui` and `davimci-tui` from diverging, and what makes
//! the cross-frontend parity test meaningful.

use davimci_cmd::Session;
use davimci_core::{ClipId, Frame, TrackId, TrackKind};
use davimci_keys::Mode;
use davimci_keys::mode::VisualSelection;
use davimci_motion::{JumpConfig, JumpPoints, Zoom};

use crate::job::Job;
use crate::message::Message;
use crate::viewport::Viewport;

/// Everything the app knows that is not derivable from the session: mode,
/// pending input, and the command line. Assembled by [`crate::App`], or by a
/// test that wants a specific view.
#[derive(Debug, Clone)]
pub struct ViewInputs<'a> {
    pub mode: Mode,
    pub selection: Option<&'a VisualSelection>,
    /// Keys typed so far in an unfinished sequence, e.g. `"3d"`.
    pub pending: String,
    /// Contents of the `:` line while in `COMMAND` mode.
    pub command_line: Option<String>,
    pub message: Option<Message>,
    pub job: Option<Job>,
    pub recording: Option<char>,
}

impl Default for ViewInputs<'_> {
    /// The default view is `NORMAL` with nothing pending; `davimci_keys::Mode`
    /// has no `Default` of its own because no other mode would be a sane one.
    fn default() -> Self {
        Self {
            mode: Mode::Normal,
            selection: None,
            pending: String::new(),
            command_line: None,
            message: None,
            job: None,
            recording: None,
        }
    }
}

/// One tick on the ruler: a jump point that is currently on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub frame: Frame,
    pub column: u32,
    /// Ticks at a clip boundary are drawn taller than subdivision ticks.
    pub major: bool,
}

/// A clip as drawn: timeline facts plus where they land on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipView {
    pub id: ClipId,
    pub label: String,
    pub start: Frame,
    pub end: Frame,
    /// Inclusive column range, clamped to the viewport. `None` when the clip
    /// is entirely off-screen (such clips are not emitted at all).
    pub columns: (u32, u32),
    pub selected: bool,
    pub offline: bool,
    pub linked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackView {
    pub index: usize,
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub muted: bool,
    pub solo: bool,
    pub locked: bool,
    pub focused: bool,
    /// True when some other track is soloed, so this one is silent by effect
    /// (spec §6.1).
    pub silenced_by_solo: bool,
    pub clips: Vec<ClipView>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayheadView {
    pub frame: Frame,
    pub track: TrackId,
    /// `None` only if the viewport has not been followed yet.
    pub column: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionView {
    pub start: Frame,
    /// Exclusive.
    pub end: Frame,
    pub tracks: Vec<TrackId>,
    pub columns: Option<(u32, u32)>,
}

/// The complete, frontend-agnostic description of one frame of UI.
#[derive(Debug, Clone)]
pub struct ViewState {
    pub mode: Mode,
    /// Rendered exactly as spec §2 shows it: `-- VISUAL (V1,A2) --`.
    pub mode_line: String,
    pub viewport: Viewport,
    pub visible_range: (Frame, Frame),
    pub duration: Frame,
    pub ticks: Vec<Tick>,
    pub tracks: Vec<TrackView>,
    pub playhead: PlayheadView,
    pub selection: Option<SelectionView>,
    pub pending: String,
    pub command_line: Option<String>,
    pub message: Option<Message>,
    pub job: Option<Job>,
    pub recording: Option<char>,
}

impl ViewState {
    /// Project a session onto the screen.
    #[must_use]
    pub fn build(
        session: &Session,
        viewport: Viewport,
        jump_cfg: &JumpConfig,
        inputs: &ViewInputs<'_>,
    ) -> Self {
        let tl = session.timeline();
        let playhead = tl.playhead();
        let (from, to) = viewport.visible_range();
        let any_solo = tl.tracks().iter().any(|t| t.solo);

        let selection = inputs.selection.map(|sel| {
            let range = sel.range();
            SelectionView {
                start: range.start,
                end: range.end,
                tracks: sel.tracks.clone(),
                columns: column_span(&viewport, range.start, range.end),
            }
        });

        let selected_clip = |track: TrackId, start: Frame, end: Frame| -> bool {
            selection
                .as_ref()
                .is_some_and(|s| s.tracks.contains(&track) && start < s.end && end > s.start)
        };

        let tracks = tl
            .tracks()
            .iter()
            .enumerate()
            .skip(viewport.top_track())
            .take(viewport.rows())
            .map(|(index, t)| TrackView {
                index,
                id: t.id,
                name: t.name.clone(),
                kind: t.kind,
                muted: t.muted,
                solo: t.solo,
                locked: t.locked,
                focused: t.id == playhead.track,
                silenced_by_solo: any_solo && !t.solo,
                clips: t
                    .clips()
                    .iter()
                    .filter(|c| c.end() > from && c.start < to)
                    .filter_map(|c| {
                        column_span(&viewport, c.start, c.end()).map(|columns| ClipView {
                            id: c.id,
                            label: c.label.clone(),
                            start: c.start,
                            end: c.end(),
                            columns,
                            selected: selected_clip(t.id, c.start, c.end()),
                            offline: c.is_offline(),
                            linked: c.group.is_some(),
                        })
                    })
                    .collect(),
            })
            .collect();

        let ticks = ruler_ticks(tl, viewport, jump_cfg);

        Self {
            mode: inputs.mode,
            mode_line: mode_line(tl, inputs),
            visible_range: (from, to),
            duration: tl.duration(),
            viewport,
            ticks,
            tracks,
            playhead: PlayheadView {
                frame: playhead.frame,
                track: playhead.track,
                column: viewport.column_of(playhead.frame),
            },
            selection,
            pending: inputs.pending.clone(),
            command_line: inputs.command_line.clone(),
            message: inputs.message.clone(),
            job: inputs.job.clone(),
            recording: inputs.recording,
        }
    }

    /// A stable textual dump, used as the golden input for every frontend's
    /// rendering tests. Changing this format is a deliberate act: it fails
    /// the snapshot tests in `davimci-app` *and* in the frontends, which is
    /// the point.
    #[must_use]
    pub fn dump(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "{}", self.mode_line);
        let _ = writeln!(
            s,
            "viewport zoom={} cols={} rows={} range={}..{} duration={}",
            self.viewport.zoom().level(),
            self.viewport.columns(),
            self.viewport.rows(),
            self.visible_range.0.get(),
            self.visible_range.1.get(),
            self.duration.get()
        );
        let _ = write!(s, "ruler");
        for t in &self.ticks {
            let _ = write!(s, " {}{}", t.column, if t.major { "!" } else { "." });
        }
        s.push('\n');
        for t in &self.tracks {
            let _ = write!(
                s,
                "{}{} {}{}{}{}",
                if t.focused { ">" } else { " " },
                t.index,
                t.name,
                if t.muted { " muted" } else { "" },
                if t.solo { " solo" } else { "" },
                if t.silenced_by_solo { " silenced" } else { "" },
            );
            for c in &t.clips {
                let _ = write!(
                    s,
                    " [{}:{}..{}@{}-{}{}{}{}]",
                    c.id.get(),
                    c.start.get(),
                    c.end.get(),
                    c.columns.0,
                    c.columns.1,
                    if c.selected { " sel" } else { "" },
                    if c.offline { " offline" } else { "" },
                    if c.linked { " linked" } else { "" },
                );
            }
            s.push('\n');
        }
        let _ = writeln!(
            s,
            "playhead frame={} track={} col={}",
            self.playhead.frame.get(),
            self.playhead.track.get(),
            self.playhead
                .column
                .map_or_else(|| "-".to_string(), |c| c.to_string())
        );
        if let Some(sel) = &self.selection {
            let names: Vec<String> = sel.tracks.iter().map(|t| t.get().to_string()).collect();
            let _ = writeln!(
                s,
                "selection {}..{} tracks={}",
                sel.start.get(),
                sel.end.get(),
                names.join(",")
            );
        }
        if let Some(cmd) = &self.command_line {
            let _ = writeln!(s, "cmdline :{cmd}");
        }
        if !self.pending.is_empty() {
            let _ = writeln!(s, "pending {}", self.pending);
        }
        if let Some(r) = self.recording {
            let _ = writeln!(s, "recording {r}");
        }
        if let Some(j) = &self.job {
            let _ = writeln!(s, "job {} {}%", j.label, j.percent());
        }
        if let Some(m) = &self.message {
            let _ = writeln!(s, "message {:?} {}", m.severity, m.text);
        }
        s
    }
}

fn column_span(viewport: &Viewport, start: Frame, end: Frame) -> Option<(u32, u32)> {
    let (from, to) = viewport.visible_range();
    if end <= from || start >= to {
        return None;
    }
    let first = viewport.column_of(start.max(from))?;
    // `end` is exclusive; the last drawn column is the one holding `end - 1`.
    let last_frame = Frame(end.get().saturating_sub(1)).min(Frame(to.get().saturating_sub(1)));
    let last = viewport.column_of(last_frame.max(start.max(from)))?;
    Some((first, last.max(first)))
}

fn ruler_ticks(tl: &davimci_core::Timeline, viewport: Viewport, cfg: &JumpConfig) -> Vec<Tick> {
    let major: Vec<Frame> = {
        let mut cfg_major = *cfg;
        cfg_major.subdivide_from = u8::MAX;
        JumpPoints::build(tl, Some(tl.playhead().track), Zoom::OUT, &cfg_major, &[])
            .points()
            .to_vec()
    };
    let all = JumpPoints::build(tl, Some(tl.playhead().track), viewport.zoom(), cfg, &[]);
    let mut ticks: Vec<Tick> = Vec::new();
    for f in all.points() {
        let Some(column) = viewport.column_of(*f) else {
            continue;
        };
        let tick = Tick {
            frame: *f,
            column,
            major: major.binary_search(f).is_ok(),
        };
        // Two jump points can quantise onto one column when zoomed out; the
        // taller tick wins so a clip boundary never disappears behind a
        // subdivision.
        match ticks.last_mut() {
            Some(prev) if prev.column == column => prev.major |= tick.major,
            _ => ticks.push(tick),
        }
    }
    ticks
}

fn mode_line(tl: &davimci_core::Timeline, inputs: &ViewInputs<'_>) -> String {
    let name = |id: TrackId| {
        tl.track(id)
            .map_or_else(|| format!("?{}", id.get()), |t| t.name.clone())
    };
    let scope = match inputs.selection {
        Some(sel) if inputs.mode.is_visual() => sel
            .tracks
            .iter()
            .map(|t| name(*t))
            .collect::<Vec<_>>()
            .join(","),
        _ => name(tl.playhead().track),
    };
    let label = match inputs.mode {
        Mode::Normal => "NORMAL",
        Mode::Visual => "VISUAL",
        Mode::VisualLine => "VISUAL-LINE",
        Mode::VisualBlock => "VISUAL-BLOCK",
        Mode::Insert => "INSERT",
        Mode::Command => "COMMAND",
    };
    format!("-- {label} ({scope}) --")
}
