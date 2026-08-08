//! Drawing a [`ViewState`] into terminal rows.
//!
//! Everything here is a pure function of the view plus the terminal size: no
//! terminal, no cursor, no I/O. That is what makes the snapshot tests run
//! anywhere, and it keeps the rule that a frontend draws what the app decided
//! and decides nothing itself - every column, tick and label in these rows
//! was computed in `davimci-app`.
//!
//! The terminal is coarser than the window by construction: one row per
//! track, one cell per timeline column, no filmstrips and no in-video
//! overlays.

use std::fmt::Write as _;

use davimci_app::{
    LabelMetrics, MediaPicker, Numbers, PanelContent, PanelRole, PanelView, PickerIntent,
    SubtitleEdit, Surface, ViewState,
};

use crate::preview::Band;
use davimci_core::TrackKind;
use ratatui::prelude::{Line, Span, Style};
use ratatui::style::Color;

/// Width of the track-name gutter, separator included.
pub const GUTTER: u16 = 10;

/// The fill the ruler is drawn on; a label may only cover this, never a tick.
const RULE: char = '\u{2500}';

/// Rows the chrome takes: the ruler and the status line.
const CHROME_ROWS: u16 = 2;

/// The modals a terminal frontend can have open, as the renderer needs them.
#[derive(Debug, Default, Clone, Copy)]
pub struct Overlay<'a> {
    pub picker: Option<&'a MediaPicker>,
    pub subtitle: Option<&'a SubtitleEdit>,
}

impl Overlay<'_> {
    #[must_use]
    fn is_open(&self) -> bool {
        self.picker.is_some() || self.subtitle.is_some()
    }
}

/// The timeline area a terminal of this size offers.
///
/// The `:` line, its completions and the preview band take rows from the
/// tracks, so the app is told about a smaller timeline the moment any of them
/// opens - a frontend that kept the old count would draw a track over its own
/// command line.
#[must_use]
pub fn surface(width: u16, height: u16, command_rows: u16, preview_rows: u16) -> Surface {
    Surface {
        columns: u32::from(width.saturating_sub(GUTTER)),
        rows: usize::from(
            height
                .saturating_sub(CHROME_ROWS)
                .saturating_sub(command_rows)
                .saturating_sub(preview_rows)
                .max(1),
        ),
        // A terminal cell cannot hold a picture, so no clip is ever sampled
        // for a filmstrip.
        thumbnail_columns: 0,
        // A terminal column *is* a cell.
        cell_columns: u32::from(width.saturating_sub(GUTTER)),
        // A panel may cover the ruler, the preview band and the lanes -
        // everything above the status line. Only the status and command
        // rows are kept clear, so a which-key list is cut off by the
        // terminal rather than by the track count.
        cell_rows: u32::from(height.saturating_sub(1).saturating_sub(command_rows).max(1)),
    }
}

/// How many rows the `:` line wants: none while it is closed, one when it is
/// open, two when it is suggesting completions.
#[must_use]
pub fn command_rows(view: &ViewState) -> u16 {
    match &view.command_line {
        None => 0,
        Some(c) if c.completions.is_empty() => 1,
        Some(_) => 2,
    }
}

/// One screen, top row first. The preview band sits above the ruler; a
/// graphics protocol leaves its rows blank here and writes over them, which
/// is why they are still counted.
#[must_use]
pub fn lines(
    view: &ViewState,
    overlay: Overlay<'_>,
    width: u16,
    height: u16,
    band: &Band,
    numbers: Numbers,
) -> Vec<Line<'static>> {
    let columns = width.saturating_sub(GUTTER);
    let mut out: Vec<Line<'static>> = Vec::new();
    for row in 0..band.rows {
        out.push(match band.cells.get(usize::from(row)) {
            Some(line) => line.clone(),
            None => Line::from(Span::raw(fit("", width))),
        });
    }
    out.push(ruler(view, columns, numbers));

    if overlay.is_open() {
        let rows = height
            .saturating_sub(CHROME_ROWS)
            .saturating_sub(command_rows(view))
            .saturating_sub(band.rows)
            .max(1);
        out.extend(modal(overlay, width, rows));
    } else {
        for track in &view.tracks {
            out.push(lane(view, track, columns));
        }
    }

    // Panels cover the whole editing area, which is everything drawn so far:
    // the preview band, the ruler and the lanes. The status and command
    // rows are appended after this and stay clear.
    //
    // A project with fewer tracks than the terminal has room for draws fewer
    // rows than the app placed panels in, so the area is grown to what the
    // open panels reach - never past what the terminal can show. Without
    // this a panel is cut off by the *track count*, which is what made a
    // long which-key list disappear at the bottom.
    let room = height
        .saturating_sub(1)
        .saturating_sub(command_rows(view))
        .max(1);
    let wanted = view
        .panels
        .iter()
        .map(|p| p.rect.row.saturating_add(p.rect.rows))
        .max()
        .unwrap_or(0)
        .min(u32::from(room)) as usize;
    while out.len() < wanted {
        out.push(Line::from(Span::raw(fit("", width))));
    }
    overlay_panels(&mut out, view, width, 0);

    out.push(status(view, width));
    out.extend(command_line(view, width));
    out
}

/// Draw every placed panel over the rows beneath it.
///
/// Placement was decided in `davimci-app`; this only blits, which is why the
/// GUI and the terminal cannot put a panel in two different places.
fn overlay_panels(out: &mut [Line<'static>], view: &ViewState, width: u16, top: usize) {
    for panel in &view.panels {
        let cells = panel_rows(panel);
        for (i, row) in cells.into_iter().enumerate() {
            let Some(target) = out.get_mut(top + panel.rect.row as usize + i) else {
                continue;
            };
            let at = usize::from(GUTTER) + panel.rect.column as usize;
            *target = splice(target, at, row, width);
        }
    }
}

/// One panel as terminal rows, each exactly as wide as the panel.
///
/// A picture is a placeholder here by design: a terminal has no pixels, and
/// the rule is to degrade locally rather than to refuse the panel.
fn panel_rows(panel: &PanelView) -> Vec<Vec<Span<'static>>> {
    let w = panel.rect.columns as usize;
    let h = panel.rect.rows as usize;
    let border = w >= 2 && h >= 2;
    let inner = if border { w - 2 } else { w };
    let frame = Style::default().fg(if panel.focus {
        Color::White
    } else {
        Color::DarkGray
    });

    let mut body: Vec<Vec<Span<'static>>> = Vec::new();
    match &panel.content {
        PanelContent::Lines(lines) => {
            for line in lines {
                body.push(
                    line.spans
                        .iter()
                        .map(|s| Span::styled(s.text.clone(), role_style(s.role)))
                        .collect(),
                );
            }
        }
        PanelContent::Pixels { width, height, .. } => body.push(vec![Span::styled(
            format!("[picture {width}x{height}]"),
            Style::default().fg(Color::DarkGray),
        )]),
    }

    let mut out: Vec<Vec<Span<'static>>> = Vec::new();
    if border {
        let title = panel.title.clone().unwrap_or_default();
        let bar: String =
            std::iter::repeat_n('\u{2500}', inner.saturating_sub(title.chars().count())).collect();
        out.push(vec![Span::styled(
            fit(
                &format!("\u{250c}{title}{bar}\u{2510}"),
                u16::try_from(w).unwrap_or(u16::MAX),
            ),
            frame,
        )]);
    }
    let body_rows = if border { h.saturating_sub(2) } else { h };
    for i in 0..body_rows {
        let content = body.get(i).cloned().unwrap_or_default();
        let mut row = Vec::new();
        if border {
            row.push(Span::styled("\u{2502}".to_string(), frame));
        }
        let mut used = 0usize;
        for span in content {
            let room = inner.saturating_sub(used);
            if room == 0 {
                break;
            }
            let text: String = span.content.chars().take(room).collect();
            used += text.chars().count();
            row.push(Span::styled(text, span.style));
        }
        row.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        if border {
            row.push(Span::styled("\u{2502}".to_string(), frame));
        }
        out.push(row);
    }
    if border {
        out.push(vec![Span::styled(
            format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(inner)),
            frame,
        )]);
    }
    out.truncate(h);
    out
}

fn role_style(role: PanelRole) -> Style {
    match role {
        PanelRole::Normal => Style::default().fg(Color::Gray),
        PanelRole::Key => Style::default().fg(Color::Yellow).bold(),
        PanelRole::Accent => Style::default().fg(Color::White).bold(),
        PanelRole::Warning => Style::default().fg(Color::Red),
    }
}

/// Overwrite `line` from character `at` with `patch`, keeping the styles of
/// everything it does not cover and the row exactly `width` wide.
fn splice(line: &Line<'static>, at: usize, patch: Vec<Span<'static>>, width: u16) -> Line<'static> {
    // Cell by cell rather than span by span: a panel lands wherever it lands,
    // and a row that came back a character short or long would ruin every
    // row beneath it.
    let mut cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|s| s.content.chars().map(move |c| (c, s.style)))
        .collect();
    cells.resize(usize::from(width), (' ', Style::default()));
    let mut column = at;
    for span in patch {
        for c in span.content.chars() {
            if let Some(cell) = cells.get_mut(column) {
                *cell = (c, span.style);
            }
            column += 1;
        }
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    for (c, style) in cells {
        match out.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => out.push(Span::styled(c.to_string(), style)),
        }
    }
    Line::from(out)
}

/// The ruler: jump points as ticks, with the playhead's own column marked,
/// and the numbers `:set numbers` asked for written beside them.
fn ruler(view: &ViewState, columns: u16, numbers: Numbers) -> Line<'static> {
    let mut cells = vec![RULE; usize::from(columns)];
    for tick in &view.ticks {
        if let Some(cell) = cells.get_mut(tick.column as usize) {
            *cell = if tick.major { '\u{253c}' } else { '\u{252c}' };
        }
    }
    if let Some(column) = view.playhead.column
        && let Some(cell) = cells.get_mut(column as usize)
    {
        *cell = '\u{25bc}';
    }
    label_ticks(view, &mut cells, numbers);
    Line::from(vec![
        Span::styled(gutter_text("time"), Style::default().fg(Color::DarkGray)),
        Span::styled(
            cells.into_iter().collect::<String>(),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// Write the numbers `davimci-app` decided on into the rule beside their
/// ticks. Which tick gets one is not this crate's business; a cell is one
/// unit wide, which is all the terminal contributes.
fn label_ticks(view: &ViewState, cells: &mut [char], numbers: Numbers) {
    let width = u32::try_from(cells.len()).unwrap_or(u32::MAX);
    for label in davimci_app::labels(view, numbers, LabelMetrics::cells(width)) {
        for (i, c) in label.text.chars().enumerate() {
            if let Some(cell) = cells.get_mut(label.offset as usize + i) {
                *cell = c;
            }
        }
    }
}

/// One track: its name in the gutter, its clips in the timeline columns.
fn lane(view: &ViewState, track: &davimci_app::TrackView, columns: u16) -> Line<'static> {
    let width = usize::from(columns);
    // A cell is a character plus the style it was drawn with, so clips can
    // be told apart without one span per column in the common case.
    let mut cells: Vec<(char, Style)> = vec![(' ', Style::default()); width];

    for clip in &track.clips {
        let (first, last) = clip.columns;
        let style = clip_style(track, clip);
        let body = if clip.offline { '\u{2591}' } else { '\u{2588}' };
        for column in first..=last {
            if let Some(cell) = cells.get_mut(column as usize) {
                *cell = (body, style);
            }
        }
        // Audio lanes draw their envelope inside the clip, so a level of
        // zero reads as silence rather than as "no clip here".
        if track.kind == TrackKind::Audio {
            for column in first..=last {
                let Some(level) = track.waveform.get(column as usize) else {
                    continue;
                };
                if let Some(cell) = cells.get_mut(column as usize) {
                    cell.0 = level_char(*level);
                }
            }
        }
        // The label goes over the clip when it fits, which is what makes a
        // wide clip identifiable without a properties panel.
        let room = last.saturating_sub(first) as usize + 1;
        if room >= 3 {
            for (i, c) in clip.label.chars().take(room - 1).enumerate() {
                if let Some(cell) = cells.get_mut(first as usize + 1 + i) {
                    *cell = (c, style);
                }
            }
        }
    }

    // The selection is a region, not a set of clips: invert exactly the
    // columns it covers, so half a clip reads as half selected and an empty
    // stretch of a covered lane still reads as selected.
    if let Some(sel) = &view.selection
        && sel.tracks.contains(&track.id)
        && let Some((first, last)) = sel.columns
    {
        for column in first..=last {
            if let Some(cell) = cells.get_mut(column as usize) {
                cell.1 = cell.1.reversed();
            }
        }
    }

    // One time for the whole timeline, so the playhead runs down every lane;
    // the lane it is bright on is the one an edit would land on.
    if let Some(column) = view.playhead.column
        && let Some(cell) = cells.get_mut(column as usize)
    {
        *cell = if track.focused {
            ('\u{2502}', Style::default().fg(Color::Yellow).bold())
        } else {
            ('\u{2502}', Style::default().fg(Color::DarkGray))
        };
    }

    let mut spans = vec![Span::styled(
        gutter_text(&gutter_label(track)),
        lane_style(track),
    )];
    // Runs of one style become one span: a redraw of a 200-column timeline
    // should not cost 200 of them.
    let mut run = String::new();
    let mut run_style = Style::default();
    for (c, style) in cells {
        if style != run_style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
        }
        run_style = style;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    Line::from(spans)
}

/// Grouping is a shade, not a colour: a clip's colour says whether it is
/// video or audio, and a group holds one of each.
fn clip_style(track: &davimci_app::TrackView, clip: &davimci_app::ClipView) -> Style {
    let audio = track.kind == TrackKind::Audio;
    let colour = match (clip.offline, clip.linked, audio) {
        (true, _, _) => Color::Red,
        // The palette's mid-dark cyan and green: the same hue as the plain
        // clip, a step down in brightness. Dim alone does not read across a
        // lane, and the darkest shades read as black.
        (false, true, false) => Color::Indexed(30),
        (false, true, true) => Color::Indexed(28),
        (false, false, false) => Color::Cyan,
        (false, false, true) => Color::Green,
    };
    Style::default().fg(colour)
}

fn lane_style(track: &davimci_app::TrackView) -> Style {
    let base = if track.muted || track.silenced_by_solo {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::Gray)
    };
    if track.focused { base.bold() } else { base }
}

/// `V1`, plus the flags that change what the track does.
fn gutter_label(track: &davimci_app::TrackView) -> String {
    let mut s = String::new();
    s.push(if track.focused { '>' } else { ' ' });
    s.push_str(&track.name);
    if track.muted {
        s.push('m');
    }
    if track.solo {
        s.push('s');
    }
    if track.locked {
        s.push('L');
    }
    s
}

/// Envelope level as one block character; nine steps is what the glyphs give.
fn level_char(level: u8) -> char {
    const BLOCKS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let step = usize::from(level) * BLOCKS.len() / 256;
    BLOCKS[step.min(BLOCKS.len() - 1)]
}

/// The mode line, what is pending, and whatever the app last had to say.
fn status(view: &ViewState, width: u16) -> Line<'static> {
    let mut left = view.mode_line.clone();
    if let Some(r) = view.recording {
        let _ = write!(left, " recording @{r}");
    }
    if let Some(job) = &view.job {
        let _ = write!(left, " [{} {}%]", job.label, job.percent());
    }
    if let Some(m) = &view.message {
        left.push(' ');
        left.push_str(&m.text);
    }
    let right = format!(
        "{}{}/{}",
        if view.pending.is_empty() {
            String::new()
        } else {
            format!("{} ", view.pending)
        },
        view.playhead.frame.get(),
        view.duration.get()
    );
    let style = match view.message.as_ref().map(|m| m.severity) {
        Some(davimci_app::Severity::Error) => Style::default().fg(Color::Red),
        Some(davimci_app::Severity::Warning) => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::White),
    };
    Line::from(Span::styled(justify(&left, &right, width), style))
}

/// The `:` line and its suggestions, or nothing at all.
fn command_line(view: &ViewState, width: u16) -> Vec<Line<'static>> {
    let Some(cmd) = &view.command_line else {
        return Vec::new();
    };
    let mut out = vec![Line::from(Span::raw(fit(
        &format!(":{}", cmd.buffer),
        width,
    )))];
    if !cmd.completions.is_empty() {
        out.push(Line::from(Span::styled(
            fit(&cmd.completions.join(" "), width),
            Style::default().fg(Color::DarkGray),
        )));
    }
    out
}

/// A modal, drawn over the timeline rows. A terminal has no floating window,
/// so it takes the space the tracks were using and gives it back on close.
fn modal(overlay: Overlay<'_>, width: u16, rows: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    if let Some(picker) = overlay.picker {
        let title = match picker.intent() {
            PickerIntent::Insert => "insert media at the playhead",
            PickerIntent::Append => "append media after this clip",
            PickerIntent::Replace => "replace this clip with",
        };
        out.push(Line::from(Span::styled(
            fit(&format!("{title}: {}", picker.query()), width),
            Style::default().fg(Color::White).bold(),
        )));
        let visible = picker.visible();
        let selected = picker.selected();
        if visible.is_empty() {
            out.push(Line::from(Span::styled(
                fit("no media here", width),
                Style::default().fg(Color::DarkGray),
            )));
        }
        // Scroll so the selected row is always on screen, however long the
        // listing is.
        let room = usize::from(rows.saturating_sub(1)).max(1);
        let first = selected.saturating_sub(room.saturating_sub(1));
        for (i, entry) in visible.iter().enumerate().skip(first).take(room) {
            let mark = if i == selected { '>' } else { ' ' };
            let name = if entry.is_dir {
                format!("{}/", entry.label)
            } else {
                entry.label.clone()
            };
            let style = if i == selected {
                Style::default().fg(Color::White).reversed()
            } else if entry.is_dir {
                Style::default().fg(Color::Blue)
            } else {
                Style::default().fg(Color::Gray)
            };
            out.push(Line::from(Span::styled(
                fit(&format!("{mark} {name}"), width),
                style,
            )));
        }
    } else if let Some(edit) = overlay.subtitle {
        out.push(Line::from(Span::styled(
            fit("subtitle text - Esc to commit", width),
            Style::default().fg(Color::White).bold(),
        )));
        for line in edit.buffer().split('\n') {
            out.push(Line::from(Span::raw(fit(line, width))));
        }
    }
    out.truncate(usize::from(rows));
    out
}

/// The gutter cell: a label, padded or truncated, then the separator.
fn gutter_text(label: &str) -> String {
    let room = usize::from(GUTTER) - 1;
    let mut s: String = label.chars().take(room).collect();
    while s.chars().count() < room {
        s.push(' ');
    }
    s.push('\u{2502}');
    s
}

/// `left` and `right` on one row, `right` flush to the edge, `left` cut
/// first when they do not both fit.
fn justify(left: &str, right: &str, width: u16) -> String {
    let width = usize::from(width);
    let right_len = right.chars().count();
    if right_len >= width {
        return fit(right, u16::try_from(width).unwrap_or(u16::MAX));
    }
    let room = width - right_len - 1;
    let mut s: String = left.chars().take(room).collect();
    while s.chars().count() < room + 1 {
        s.push(' ');
    }
    s.push_str(right);
    s
}

/// Pad or truncate to exactly `width` cells.
fn fit(text: &str, width: u16) -> String {
    let width = usize::from(width);
    let mut s: String = text.chars().take(width).collect();
    while s.chars().count() < width {
        s.push(' ');
    }
    s
}

/// The rendered rows as plain text, which is what the snapshot and parity
/// tests compare.
#[must_use]
pub fn plain(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_view(linked: bool, offline: bool) -> davimci_app::ClipView {
        davimci_app::ClipView {
            id: davimci_core::ClipId(1),
            label: "a".into(),
            start: davimci_core::Frame(0),
            end: davimci_core::Frame(100),
            columns: (0, 9),
            selected_columns: None,
            offline,
            linked,
            thumbnails: Vec::new(),
        }
    }

    fn track_view(kind: TrackKind) -> davimci_app::TrackView {
        davimci_app::TrackView {
            index: 0,
            id: davimci_core::TrackId(1),
            name: "V1".into(),
            kind,
            muted: false,
            solo: false,
            locked: false,
            focused: false,
            silenced_by_solo: false,
            clips: Vec::new(),
            waveform: Vec::new(),
        }
    }

    /// Grouping darkens a clip; it never repaints it, because the colour is
    /// how video is told from audio.
    #[test]
    fn a_grouped_clip_keeps_the_hue_of_its_kind() {
        for (kind, plain_colour, grouped_colour) in [
            (TrackKind::Video, Color::Cyan, Color::Indexed(30)),
            (TrackKind::Audio, Color::Green, Color::Indexed(28)),
        ] {
            let track = track_view(kind);
            let plain = clip_style(&track, &clip_view(false, false));
            let grouped = clip_style(&track, &clip_view(true, false));
            assert_eq!(plain.fg, Some(plain_colour));
            assert_eq!(grouped.fg, Some(grouped_colour));
        }
        // Video and audio stay apart once grouped.
        assert_ne!(
            clip_style(&track_view(TrackKind::Video), &clip_view(true, false)),
            clip_style(&track_view(TrackKind::Audio), &clip_view(true, false))
        );
    }

    /// Offline media is the one thing louder than grouping: an offline clip
    /// blocks export, and a dimmed red would understate that.
    #[test]
    fn offline_beats_grouped() {
        let track = track_view(TrackKind::Video);
        let offline = clip_style(&track, &clip_view(true, true));
        assert_eq!(offline.fg, Some(Color::Red));
        assert_eq!(offline, clip_style(&track, &clip_view(false, true)));
    }

    #[test]
    fn the_surface_gives_the_command_line_its_rows_back() {
        let full = surface(80, 10, 0, 0);
        assert_eq!(full.columns, 80 - u32::from(GUTTER));
        assert_eq!(full.rows, 8);
        assert_eq!(surface(80, 10, 2, 0).rows, 6);
        // The preview band takes its rows the same way the `:` line does.
        assert_eq!(surface(80, 10, 2, 3).rows, 3);
        // A terminal too small for a track still claims one, rather than
        // reporting a timeline with no lanes at all.
        assert_eq!(surface(4, 1, 0, 0).rows, 1);
    }

    fn ruler_row(ticks: &[(u32, bool, i32)], playhead: Option<u32>, numbers: Numbers) -> String {
        let mut cells = vec![RULE; 40];
        for (column, major, _) in ticks {
            cells[*column as usize] = if *major { '\u{253c}' } else { '\u{252c}' };
        }
        if let Some(column) = playhead {
            cells[column as usize] = '\u{25bc}';
        }
        let view = view_with(ticks, playhead);
        label_ticks(&view, &mut cells, numbers);
        cells.into_iter().collect()
    }

    /// A golden view with the ruler facts under test substituted in.
    fn view_with(ticks: &[(u32, bool, i32)], playhead: Option<u32>) -> ViewState {
        let mut view = davimci_app::fixtures::normal();
        view.ticks = ticks
            .iter()
            .map(|(column, major, relative)| davimci_app::Tick {
                frame: davimci_core::Frame(u64::from(*column)),
                column: *column,
                major: *major,
                relative: *relative,
            })
            .collect();
        view.playhead.column = playhead;
        view
    }

    /// Which ticks are numbered is decided in `davimci-app` and tested there;
    /// what this row owes is putting those digits beside their ticks without
    /// covering a tick or the playhead.
    #[test]
    fn the_ruler_draws_the_numbers_beside_their_ticks() {
        let ticks = [(0, true, -1), (10, true, 0), (20, true, 1)];
        let bare = ruler_row(&ticks, Some(10), Numbers::Off);
        assert!(!bare.contains('1'), "{bare}");

        let row = ruler_row(&ticks, Some(10), Numbers::Relative);
        let cells: Vec<char> = row.chars().collect();
        assert_eq!(cells[0], '\u{253c}', "{row}");
        assert_eq!(
            cells[10], '\u{25bc}',
            "the playhead was written over: {row}"
        );
        assert_eq!((cells[1], cells[11], cells[21]), ('1', '0', '1'), "{row}");

        let row = ruler_row(&ticks, Some(10), Numbers::Absolute);
        let cells: Vec<char> = row.chars().collect();
        assert_eq!((cells[11], cells[12]), ('1', '0'), "{row}");
    }

    #[test]
    fn every_row_is_exactly_as_wide_as_the_terminal() {
        let text = fit("abc", 6);
        assert_eq!(text, "abc   ");
        assert_eq!(fit("abcdefgh", 3), "abc");
        assert_eq!(justify("left", "1/2", 12), "left     1/2");
    }
}
