//! The view state every frontend renders.
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
use crate::thumbnail::{Thumbnail, Thumbnails};
use crate::viewport::Viewport;
use crate::waveform::Waveforms;

/// Everything the app knows that is not derivable from the session: mode,
/// pending input, and the command line. Assembled by [`crate::App`], or by a
/// test that wants a specific view.
#[derive(Debug, Clone)]
pub struct ViewInputs<'a> {
    pub mode: Mode,
    pub selection: Option<&'a VisualSelection>,
    /// Keys typed so far in an unfinished sequence, e.g. `"3d"`.
    pub pending: String,
    /// The `:` line while in `COMMAND` mode: what has been typed, where the
    /// caret is, and what would complete it.
    pub command_line: Option<CommandLineView>,
    pub message: Option<Message>,
    pub job: Option<Job>,
    pub recording: Option<char>,
    /// Analysed audio, when the host has published any.
    pub waveforms: Option<&'a Waveforms>,
    /// Decoded clip thumbnails, when the host has published any.
    pub thumbnails: Option<&'a Thumbnails>,
    /// How wide one thumbnail is drawn, in columns - the frontend's
    /// [`crate::Surface`] hint. Zero draws no filmstrip.
    pub thumbnail_columns: u32,
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
            waveforms: None,
            thumbnails: None,
            thumbnail_columns: 0,
        }
    }
}

/// The `:` line as drawn: a frontend renders this and nothing of its own,
/// so the GUI and the TUI cannot disagree about what the user typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLineView {
    pub buffer: String,
    /// Byte offset of the caret in `buffer`.
    pub cursor: usize,
    /// Candidates matching the word being typed, in vocabulary order.
    /// Empty while the word matches nothing, and while it matches exactly
    /// one thing that is already fully typed - a suggestion list that only
    /// repeats the line is noise.
    pub completions: Vec<String>,
}

/// One tick on the ruler: a jump point that is currently on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick {
    pub frame: Frame,
    pub column: u32,
    /// Ticks at a clip boundary are drawn taller than subdivision ticks.
    pub major: bool,
    /// Distance from the jump point at or before the playhead, counted in
    /// jump points: `0` is the one the playhead sits on, `1` is the next
    /// `l` lands on, `-1` the previous. This is the count `3l` needs, shown
    /// the way vim's `relativenumber` shows lines.
    pub relative: i32,
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
    /// The clip's filmstrip: a picture per sample point, with the column it
    /// belongs at. Only the samples the host has decoded are here, so a
    /// strip fills in as it arrives and an undecoded clip is simply plain.
    /// Shared pixels, so assembling a view copies nothing.
    pub thumbnails: Vec<(u32, Thumbnail)>,
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
    ///.
    pub silenced_by_solo: bool,
    pub clips: Vec<ClipView>,
    /// Peak level per visible column, `0..=255`, for audio lanes whose
    /// source has been analysed. Empty means "nothing to draw" - either the
    /// lane carries no audio or its analysis has not landed yet.
    pub waveform: Vec<u8>,
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
    /// Rendered as `-- VISUAL (V1,A2) --`.
    pub mode_line: String,
    pub viewport: Viewport,
    pub visible_range: (Frame, Frame),
    pub duration: Frame,
    pub ticks: Vec<Tick>,
    pub tracks: Vec<TrackView>,
    pub playhead: PlayheadView,
    pub selection: Option<SelectionView>,
    pub pending: String,
    pub command_line: Option<CommandLineView>,
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
                            thumbnails: inputs.thumbnails.map_or_else(Vec::new, |store| {
                                strip_samples(&viewport, c, inputs.thumbnail_columns)
                                    .into_iter()
                                    .filter_map(|(column, source)| {
                                        store.get(c.id, source).map(|t| (column, t.clone()))
                                    })
                                    .collect()
                            }),
                        })
                    })
                    .collect(),
                waveform: inputs
                    .waveforms
                    .and_then(|w| w.get(t.id))
                    .map(|w| track_waveform(t, w, &viewport, tl.props.fps))
                    .unwrap_or_default(),
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
            let _ = write!(
                s,
                " {}{}{}",
                t.column,
                if t.major { "!" } else { "." },
                t.relative
            );
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
            let _ = writeln!(s, "cmdline :{}|{}", cmd.buffer, cmd.cursor);
            if !cmd.completions.is_empty() {
                let _ = writeln!(s, "completions {}", cmd.completions.join(" "));
            }
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

/// One lane's envelope: peak level per visible column.
///
/// A column is answered from the *clip under it*, mapped back through
/// `source_in`, because the analysis measures the source. Doing it any other
/// way makes a trimmed or slipped clip draw somebody else's audio.
fn track_waveform(
    track: &davimci_core::Track,
    waveform: &crate::waveform::Waveform,
    viewport: &Viewport,
    fps: davimci_core::Fps,
) -> Vec<u8> {
    let columns = viewport.columns();
    let mut out = vec![0u8; columns as usize];
    let ms = |frame: Frame| -> u64 {
        u64::try_from(fps.frame_to_nanos(frame) / 1_000_000).unwrap_or(u64::MAX)
    };
    for column in 0..columns {
        let start = viewport.frame_at_column(column);
        let end = viewport
            .frame_at_column(column + 1)
            .max(Frame(start.get() + 1));
        let Some(clip) = track.clip_at(start) else {
            continue;
        };
        let into_source = |f: Frame| -> Frame {
            Frame(clip.source_in.get() + f.get().saturating_sub(clip.start.get()))
        };
        let from = into_source(start);
        let to = into_source(end.min(clip.end()));
        out[column as usize] = waveform.level(ms(from), ms(to));
    }
    out
}

/// Where a clip's filmstrip is sampled: one point every `every` columns,
/// from the clip's first visible column, as `(column, source frame)`.
///
/// Sample points are anchored to the clip's own start rather than to the
/// screen, so scrolling slides the strip instead of re-cutting it - which
/// would otherwise make every scroll a screenful of fresh decodes.
#[must_use]
pub fn strip_samples(
    viewport: &Viewport,
    clip: &davimci_core::Clip,
    every: u32,
) -> Vec<(u32, Frame)> {
    let mut out = Vec::new();
    if every == 0 {
        return out;
    }
    let Some((first, last)) = column_span(viewport, clip.start, clip.end()) else {
        return out;
    };
    // The strip is laid out from the clip's start, even when that is off
    // screen, so a sample keeps its frame as the view scrolls.
    let head = viewport.column_of_unclamped(clip.start);
    let mut column = head;
    while column + i64::from(every) <= i64::from(first) {
        column += i64::from(every);
    }
    while column <= i64::from(last) {
        if column >= i64::from(first) {
            let at = viewport.frame_at_column_signed(column);
            let at = at.max(clip.start).min(Frame(clip.end().get() - 1));
            let source = Frame(clip.source_in.get() + (at.get() - clip.start.get()));
            out.push((u32::try_from(column).unwrap_or(u32::MAX), source));
        }
        column += i64::from(every);
    }
    out
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
    // Where the playhead sits in the jump-point sequence. Everything on
    // screen is numbered relative to it, so the number on a tick is the
    // count that lands there: `3l` goes to the tick labelled 3.
    let points = all.points();
    let index = |i: usize| i64::try_from(i).unwrap_or(i64::MAX);
    let here = match points.binary_search(&tl.playhead().frame) {
        Ok(i) => index(i),
        // Between two points: the one behind is 0, so the one ahead is 1.
        Err(i) => index(i) - 1,
    };
    let mut ticks: Vec<Tick> = Vec::new();
    for (i, f) in points.iter().enumerate() {
        let Some(column) = viewport.column_of(*f) else {
            continue;
        };
        let tick = Tick {
            frame: *f,
            column,
            major: major.binary_search(f).is_ok(),
            relative: i32::try_from(index(i) - here).unwrap_or(i32::MAX),
        };
        // Two jump points can quantise onto one column when zoomed out; the
        // taller tick wins so a clip boundary never disappears behind a
        // subdivision.
        match ticks.last_mut() {
            Some(prev) if prev.column == column => {
                prev.major |= tick.major;
                // Keep the number nearest the playhead: the count that
                // lands on this column is the smaller one.
                if tick.relative.abs() < prev.relative.abs() {
                    prev.relative = tick.relative;
                }
            }
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
