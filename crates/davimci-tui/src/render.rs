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

use davimci_app::{MediaPicker, PickerIntent, SubtitleEdit, Surface, ViewState};

use crate::preview::Band;
use davimci_core::TrackKind;
use ratatui::prelude::{Line, Span, Style};
use ratatui::style::Color;

/// Width of the track-name gutter, separator included.
pub const GUTTER: u16 = 10;

/// Which numbers the ruler labels its jump points with.
///
/// The terminal ruler is vim's line-number gutter turned on its side: the
/// jump points are the lines, so `Relative` prints the count a motion needs
/// (`3l` is the tick labelled `3`) and `Absolute` prints the frame it sits on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Numbers {
    #[default]
    Off,
    Absolute,
    Relative,
}

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

    out.push(status(view, width));
    out.extend(command_line(view, width));
    out
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

/// Write each tick's number into the rule to its right.
///
/// A label is only ever drawn over untouched rule cells, so it can never hide
/// a tick or the playhead: where two jump points are too close to label, the
/// tick wins and the number is dropped.
fn label_ticks(view: &ViewState, cells: &mut [char], numbers: Numbers) {
    if numbers == Numbers::Off {
        return;
    }
    for tick in &view.ticks {
        let text = match numbers {
            Numbers::Off => return,
            Numbers::Absolute => tick.frame.get().to_string(),
            Numbers::Relative => tick.relative.unsigned_abs().to_string(),
        };
        let first = tick.column as usize + 1;
        let room = cells
            .get(first..first + text.chars().count())
            .is_some_and(|span| span.iter().all(|c| *c == RULE));
        if !room {
            continue;
        }
        for (i, c) in text.chars().enumerate() {
            cells[first + i] = c;
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

    if track.focused
        && let Some(column) = view.playhead.column
        && let Some(cell) = cells.get_mut(column as usize)
    {
        *cell = ('\u{2502}', Style::default().fg(Color::Yellow).bold());
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

fn clip_style(track: &davimci_app::TrackView, clip: &davimci_app::ClipView) -> Style {
    let base = if clip.offline {
        Style::default().fg(Color::Red)
    } else if clip.linked {
        Style::default().fg(Color::Magenta)
    } else if track.kind == TrackKind::Audio {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::Cyan)
    };
    if clip.selected { base.reversed() } else { base }
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
        left.push_str(&format!(" recording @{r}"));
    }
    if let Some(job) = &view.job {
        left.push_str(&format!(" [{} {}%]", job.label, job.percent()));
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

    #[test]
    fn numbers_off_leaves_the_ruler_bare() {
        let row = ruler_row(&[(0, true, 0), (10, true, 1)], Some(0), Numbers::Off);
        assert!(!row.contains('1'), "{row}");
    }

    #[test]
    fn relative_numbers_count_jump_points_from_the_playhead_either_way() {
        let ticks = [(0, true, -1), (10, true, 0), (20, true, 1), (30, true, 2)];
        let row = ruler_row(&ticks, Some(10), Numbers::Relative);
        assert_eq!(row.chars().next(), Some('\u{253c}'));
        // The distance a motion needs is what is written, unsigned, exactly
        // as `relativenumber` shows a line above the cursor.
        for (column, want) in [(1, '1'), (11, '0'), (21, '1'), (31, '2')] {
            assert_eq!(
                row.chars().nth(column),
                Some(want),
                "column {column}: {row}"
            );
        }
    }

    #[test]
    fn absolute_numbers_are_the_frame_the_tick_sits_on() {
        let row: String = ruler_row(&[(0, true, 0), (10, true, 1)], Some(0), Numbers::Absolute);
        let cells: Vec<char> = row.chars().collect();
        assert_eq!((cells[0], cells[1]), ('\u{25bc}', '0'), "{row}");
        assert_eq!((cells[11], cells[12]), ('1', '0'), "{row}");
    }

    /// Two jump points a cell apart cannot both be labelled; the tick is the
    /// thing that must survive, so the number is what is dropped.
    #[test]
    fn a_label_never_covers_a_tick_or_the_playhead() {
        let row = ruler_row(
            &[(0, true, 0), (1, true, 1), (2, true, 2)],
            None,
            Numbers::Relative,
        );
        assert_eq!(
            row.chars().take(3).collect::<String>(),
            "\u{253c}\u{253c}\u{253c}"
        );
        // A tick in the last column has no room for its own number either.
        let row = ruler_row(&[(39, true, 0)], None, Numbers::Relative);
        assert_eq!(row.chars().filter(|c| c.is_ascii_digit()).count(), 0);
    }

    #[test]
    fn every_row_is_exactly_as_wide_as_the_terminal() {
        let text = fit("abc", 6);
        assert_eq!(text, "abc   ");
        assert_eq!(fit("abcdefgh", 3), "abc");
        assert_eq!(justify("left", "1/2", 12), "left     1/2");
    }
}
