//! Window layout and timeline painting.
//!
//! The window is one column: video pane on top, timeline below it, then the
//! status line and (when open) the command line. Every size here is derived,
//! never stored, so an extreme window size produces a small layout rather
//! than an inconsistent one.

use davimci_app::{LabelMetrics, Surface, ViewState};

use crate::paint::{Chrome, DrawList, Fill, PickerView, Rect, TextRole, status_text};

/// Left padding a shell leaves inside a text box, in pixels. Layout has to
/// allow for it: a box sized to its glyphs alone loses its last character.
pub const TEXT_PADDING: u32 = 4;

/// A clip narrower or shorter than this has no room for a thumbnail that
/// would read as a picture rather than as noise.
const MIN_THUMBNAIL_SIDE: u32 = 4;

/// Vertical gap between the caret and the edges of the `:` line.
const CARET_INSET: u32 = 2;

/// Fixed metrics. A theme may change these; nothing else may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub row_height: u32,
    pub ruler_height: u32,
    pub status_height: u32,
    pub command_height: u32,
    pub track_header_width: u32,
    /// Fraction of the window height the video pane wants, in percent.
    pub video_percent: u32,
    /// Advance of one monospace character on the `:` line, used to place the
    /// caret. Text is measured by the shell's font, but a caret has to be a
    /// rectangle somewhere.
    pub char_width: u32,
    /// Advance of one digit in the ruler's smaller number font, used to
    /// decide which relative numbers fit without overlapping.
    pub number_char_width: u32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            row_height: 40,
            ruler_height: 20,
            status_height: 20,
            command_height: 20,
            track_header_width: 80,
            video_percent: 50,
            char_width: 8,
            number_char_width: 6,
        }
    }
}

/// Where each region of the window is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub window: Rect,
    pub video: Rect,
    pub ruler: Rect,
    pub tracks: Rect,
    pub headers: Rect,
    pub status: Rect,
    pub command: Option<Rect>,
    /// The suggestion row above the `:` line, when there is anything to
    /// suggest.
    pub completions: Option<Rect>,
    pub metrics: Metrics,
}

impl Layout {
    /// Lay out a window of `width` x `height` pixels.
    ///
    /// Panes are given away in priority order - status line, command line,
    /// ruler, video, timeline - so a window too short for everything loses
    /// the timeline's height rather than producing negative sizes.
    #[must_use]
    pub fn compute(
        width: u32,
        height: u32,
        metrics: Metrics,
        command_open: bool,
        completions_shown: bool,
    ) -> Self {
        let window = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut remaining = height;
        let take = |remaining: &mut u32, want: u32| -> u32 {
            let got = want.min(*remaining);
            *remaining -= got;
            got
        };

        let status_h = take(&mut remaining, metrics.status_height);
        let command_h = if command_open {
            take(&mut remaining, metrics.command_height)
        } else {
            0
        };
        let completion_h = if command_open && completions_shown {
            take(&mut remaining, metrics.command_height)
        } else {
            0
        };
        let ruler_h = take(&mut remaining, metrics.ruler_height);
        let video_h = take(
            &mut remaining,
            height.saturating_mul(metrics.video_percent.min(100)) / 100,
        );
        let tracks_h = remaining;

        let header_w = metrics.track_header_width.min(width);
        let mut y = 0;
        let video = Rect {
            x: 0,
            y,
            width,
            height: video_h,
        };
        y += video_h as i32;
        let ruler = Rect {
            x: header_w as i32,
            y,
            width: width.saturating_sub(header_w),
            height: ruler_h,
        };
        y += ruler_h as i32;
        let headers = Rect {
            x: 0,
            y,
            width: header_w,
            height: tracks_h,
        };
        let tracks = Rect {
            x: header_w as i32,
            y,
            width: width.saturating_sub(header_w),
            height: tracks_h,
        };
        y += tracks_h as i32;
        let completions = if completion_h > 0 {
            Some(Rect {
                x: 0,
                y,
                width,
                height: completion_h,
            })
        } else {
            None
        };
        y += completion_h as i32;
        let command = if command_open {
            Some(Rect {
                x: 0,
                y,
                width,
                height: command_h,
            })
        } else {
            None
        };
        y += command_h as i32;
        let status = Rect {
            x: 0,
            y,
            width,
            height: status_h,
        };

        Self {
            window,
            video,
            ruler,
            tracks,
            headers,
            status,
            command,
            completions,
            metrics,
        }
    }

    /// What the app should size its viewport to: one column per pixel of
    /// timeline width, one row per track lane that fits.
    #[must_use]
    pub fn surface(&self) -> Surface {
        Surface {
            columns: self.tracks.width.max(1),
            rows: (self.tracks.height / self.metrics.row_height.max(1)).max(1) as usize,
            // How wide one thumbnail lands: a lane's height at the assumed
            // 16:9 of the picture. The app samples a clip that often, so
            // the strip tiles without gaps or overlap.
            thumbnail_columns: (self.metrics.row_height.max(1) * 16 / 9).max(1),
        }
    }

    /// Y of track lane `row` (0 = topmost visible track).
    #[must_use]
    pub fn lane_y(&self, row: usize) -> i32 {
        self.tracks
            .y
            .saturating_add((row as i32).saturating_mul(self.metrics.row_height as i32))
    }
}

/// Paint one frame. Pure: same view plus same layout gives the same list.
#[must_use]
pub fn paint(view: &ViewState, layout: &Layout, chrome: &Chrome) -> DrawList {
    let mut d = DrawList::default();
    d.rect(layout.window, Fill::Background);
    paint_video(&mut d, layout, chrome);
    paint_ruler(&mut d, layout, view, chrome);
    paint_lanes(&mut d, layout, view);
    paint_selection(&mut d, layout, view);
    paint_playhead(&mut d, layout, view);
    paint_status(&mut d, layout, view);
    paint_command_line(&mut d, layout, view);
    // The picker is modal: it owns the keyboard, so it is drawn over
    // everything and drawn last.
    if let Some(picker) = &chrome.picker {
        paint_picker(&mut d, layout, picker);
    }
    d
}

/// The video pane. The presenter has already letterboxed, so the shell only
/// places the quad it was handed.
fn paint_video(d: &mut DrawList, layout: &Layout, chrome: &Chrome) {
    d.rect(layout.video, Fill::Background);
    let Some(q) = chrome.video else { return };
    d.rect(
        Rect {
            x: layout.video.x.saturating_add(q.x as i32),
            y: layout.video.y.saturating_add(q.y as i32),
            width: q.width,
            height: q.height,
        },
        Fill::Video,
    );
    if let Some(tc) = q.timecode {
        d.text(layout.video, TextRole::Timecode, tc);
    }
}

/// The ruler. Numbers go down first so a tick is never hidden behind one.
fn paint_ruler(d: &mut DrawList, layout: &Layout, view: &ViewState, chrome: &Chrome) {
    d.rect(layout.ruler, Fill::Ruler);
    paint_numbers(d, layout, view, chrome);
    for tick in &view.ticks {
        let height = if tick.major {
            layout.ruler.height
        } else {
            layout.ruler.height / 2
        };
        d.rect(
            Rect {
                x: layout.ruler.x.saturating_add(tick.column as i32),
                y: layout
                    .ruler
                    .y
                    .saturating_add(layout.ruler.height.saturating_sub(height) as i32),
                width: 1,
                height,
            },
            if tick.major {
                Fill::TickMajor
            } else {
                Fill::TickMinor
            },
        );
    }
}

/// One row per visible track: lane, header, clips, waveform, labels.
fn paint_lanes(d: &mut DrawList, layout: &Layout, view: &ViewState) {
    let row_h = layout.metrics.row_height;
    for (row, track) in view.tracks.iter().enumerate() {
        let y = layout.lane_y(row);
        if y >= layout.tracks.y.saturating_add(layout.tracks.height as i32) {
            break;
        }
        let lane = Rect {
            x: layout.tracks.x,
            y,
            width: layout.tracks.width,
            height: row_h,
        };
        d.rect(
            lane,
            if track.focused {
                Fill::TrackLaneFocused
            } else {
                Fill::TrackLane
            },
        );
        let header = Rect {
            x: layout.headers.x,
            y,
            width: layout.headers.width,
            height: row_h,
        };
        d.rect(header, Fill::TrackHeader);
        let mut name = track.name.clone();
        if track.muted {
            name.push('M');
        }
        if track.solo {
            name.push('S');
        }
        d.text(header, TextRole::TrackName, name);

        for clip in &track.clips {
            paint_clip(d, layout, clip, y, row_h);
        }

        // Waveform, drawn over the clips it belongs to: an envelope beside
        // the audio it describes is the whole point of showing it.
        paint_waveform(d, layout, track, y, row_h);

        // Labels last, so a clip is still identifiable on a lane whose
        // waveform would otherwise scribble over its own name.
        for clip in &track.clips {
            d.text(
                clip_rect(layout, clip, y, row_h),
                TextRole::ClipLabel,
                clip.label.clone(),
            );
        }
    }
}

fn clip_rect(layout: &Layout, clip: &davimci_app::ClipView, y: i32, row_h: u32) -> Rect {
    let (first, last) = clip.columns;
    Rect {
        x: layout.tracks.x.saturating_add(first as i32),
        y: y.saturating_add(1),
        width: last.saturating_sub(first).saturating_add(1),
        height: row_h.saturating_sub(2),
    }
}

fn paint_clip(d: &mut DrawList, layout: &Layout, clip: &davimci_app::ClipView, y: i32, row_h: u32) {
    let rect = clip_rect(layout, clip, y, row_h);
    let fill = if clip.offline {
        Fill::ClipOffline
    } else if clip.selected {
        Fill::ClipSelected
    } else if clip.linked {
        Fill::ClipLinked
    } else {
        Fill::Clip
    };
    d.rect(rect, fill);
    paint_filmstrip(d, layout, clip, rect);
}

/// One picture per sample point, each of the media at *that* point, so a long
/// clip reads as the shot changing rather than as one frame stamped over and
/// over. The app chose the sample columns; a tile is cropped at the clip's
/// edge rather than spilling onto the neighbour it is not a picture of.
fn paint_filmstrip(d: &mut DrawList, layout: &Layout, clip: &davimci_app::ClipView, rect: Rect) {
    if rect.width <= MIN_THUMBNAIL_SIDE || rect.height <= MIN_THUMBNAIL_SIDE {
        return;
    }
    let end = rect.x.saturating_add(rect.width as i32);
    let height = rect.height;
    for (column, thumb) in &clip.thumbnails {
        let tile = (thumb.width * height / thumb.height.max(1)).max(1);
        let x = layout.tracks.x.saturating_add(*column as i32).max(rect.x);
        if x >= end {
            continue;
        }
        let width = tile.min((end - x) as u32);
        d.image(
            Rect {
                x,
                y: rect.y,
                width,
                height,
            },
            clip.id,
            thumb.clone(),
            tile,
        );
    }
}

/// The selection band, drawn over the lanes it covers.
fn paint_selection(d: &mut DrawList, layout: &Layout, view: &ViewState) {
    let Some(sel) = &view.selection else { return };
    let Some((first, last)) = sel.columns else {
        return;
    };
    for (row, track) in view.tracks.iter().enumerate() {
        if !sel.tracks.contains(&track.id) {
            continue;
        }
        d.rect(
            Rect {
                x: layout.tracks.x.saturating_add(first as i32),
                y: layout.lane_y(row),
                width: last.saturating_sub(first).saturating_add(1),
                height: layout.metrics.row_height,
            },
            Fill::Selection,
        );
    }
}

/// One pixel through the ruler and every lane.
fn paint_playhead(d: &mut DrawList, layout: &Layout, view: &ViewState) {
    let Some(col) = view.playhead.column else {
        return;
    };
    d.rect(
        Rect {
            x: layout.tracks.x.saturating_add(col as i32),
            y: layout.ruler.y,
            width: 1,
            height: layout.ruler.height.saturating_add(layout.tracks.height),
        },
        Fill::Playhead,
    );
}

fn paint_status(d: &mut DrawList, layout: &Layout, view: &ViewState) {
    d.rect(layout.status, Fill::StatusLine);
    d.text(layout.status, TextRole::Status, status_text(view));
}

fn paint_command_line(d: &mut DrawList, layout: &Layout, view: &ViewState) {
    let (Some(rect), Some(line)) = (layout.command, view.command_line.as_ref()) else {
        return;
    };
    d.rect(rect, Fill::CommandLine);
    d.text(rect, TextRole::Command, format!(":{}", line.buffer));
    let cw = layout.metrics.char_width.max(1);
    paint_caret(
        d,
        rect,
        line.buffer[..line.cursor.min(line.buffer.len())]
            .chars()
            .count(),
        cw,
    );
    // Suggestions for the word being typed, on their own row above the
    // line - the app decided what they are, so both frontends show the
    // same list.
    if let Some(row) = layout.completions
        && !line.completions.is_empty()
    {
        d.rect(row, Fill::CommandLine);
        d.text(
            row,
            TextRole::Completion,
            fit_completions(&line.completions, row.width, cw),
        );
    }
}

/// The caret is a rectangle, because the shell owns the font and the layout
/// owns the geometry: `:` plus the characters before the cursor, in monospace
/// advances.
fn paint_caret(d: &mut DrawList, rect: Rect, chars_before_cursor: usize, char_width: u32) {
    let columns = (chars_before_cursor as u32).saturating_add(1);
    d.rect(
        Rect {
            x: rect
                .x
                .saturating_add(TEXT_PADDING as i32)
                .saturating_add((columns * char_width) as i32),
            y: rect.y.saturating_add(CARET_INSET as i32),
            width: 1.max(char_width / 8),
            height: rect.height.saturating_sub(2 * CARET_INSET),
        },
        Fill::Caret,
    );
}

/// Jump-point numbers on the ruler, as `:set numbers` asked for.
///
/// Which tick is numbered and what it reads is decided in `davimci-app`, the
/// same call the terminal ruler makes; the window only says how wide a digit
/// is. The padding is part of that measurement, because a number drawn into a
/// box its own glyphs wide loses its last digit.
fn paint_numbers(d: &mut DrawList, layout: &Layout, view: &ViewState, chrome: &Chrome) {
    let digit = layout.metrics.number_char_width.max(1);
    let metrics = LabelMetrics {
        width: layout.ruler.width,
        gap: 2,
        digit,
        padding: TEXT_PADDING,
        separation: digit,
        // The ticks are painted after the numbers, so a long number crossing
        // one is still a legible number over a visible tick.
        cross_ticks: true,
    };
    for label in davimci_app::labels(view, chrome.numbers, metrics) {
        d.text(
            Rect {
                x: layout.ruler.x.saturating_add(label.offset as i32),
                y: layout.ruler.y,
                width: label.width,
                height: layout.ruler.height,
            },
            TextRole::RulerNumber,
            label.text,
        );
    }
}

/// As many suggestions as fit on one row, oldest-first, with a count when
/// some are left out - a list that runs off the edge is worse than a short
/// one that says so.
fn fit_completions(completions: &[String], width: u32, char_width: u32) -> String {
    let columns = (width / char_width.max(1)).max(8) as usize;
    let mut out = String::new();
    let mut shown = 0;
    for c in completions {
        let want = if out.is_empty() { c.len() } else { c.len() + 2 };
        // Leave room for a "+n more" tail.
        if out.len() + want > columns.saturating_sub(8) {
            break;
        }
        if !out.is_empty() {
            out.push_str("  ");
        }
        out.push_str(c);
        shown += 1;
    }
    if shown < completions.len() {
        if !out.is_empty() {
            out.push_str("  ");
        }
        out.push_str(&format!("+{} more", completions.len() - shown));
    }
    out
}

/// Draw the media picker centred over the window.
/// One lane's audio envelope: a centred bar per column, height proportional
/// to the analysed peak. Integral arithmetic, so two frontends drawing the
/// same view state agree exactly.
fn paint_waveform(
    d: &mut DrawList,
    layout: &Layout,
    track: &davimci_app::TrackView,
    y: i32,
    row_h: u32,
) {
    if track.waveform.is_empty() || row_h < 4 {
        return;
    }
    let max_h = row_h.saturating_sub(2);
    let mid = y.saturating_add((row_h / 2) as i32);
    for (column, level) in track.waveform.iter().enumerate() {
        if *level == 0 || column as u32 >= layout.tracks.width {
            continue;
        }
        let height = (u32::from(*level) * max_h / u32::from(davimci_app::waveform::LEVELS)).max(1);
        d.rect(
            Rect {
                x: layout.tracks.x.saturating_add(column as i32),
                y: mid.saturating_sub((height / 2) as i32),
                width: 1,
                height,
            },
            Fill::Waveform,
        );
    }
}

fn paint_picker(d: &mut DrawList, layout: &Layout, picker: &PickerView) {
    let row_h = layout.metrics.row_height.max(1);
    let panel = centred(layout.window, row_h);
    d.rect(panel, Fill::ModalBackground);

    let mut y = panel.y;
    let line = |y: i32| Rect {
        x: panel.x,
        y,
        width: panel.width,
        height: row_h,
    };
    d.text(line(y), TextRole::ModalTitle, picker.title.clone());
    y += row_h as i32;
    d.text(line(y), TextRole::ModalQuery, format!("/{}", picker.query));
    y += row_h as i32;

    // Only as many rows as fit; the list scrolls to keep the cursor visible,
    // so a selection is never off-panel.
    let rows = ((panel.height / row_h).saturating_sub(2)).max(1) as usize;
    let first = picker.selected.saturating_sub(rows.saturating_sub(1));
    for (i, entry) in picker.entries.iter().enumerate().skip(first).take(rows) {
        let rect = line(y);
        let selected = i == picker.selected;
        if selected {
            d.rect(rect, Fill::ModalSelected);
        }
        let role = if selected {
            TextRole::ModalEntrySelected
        } else if entry.is_dir {
            TextRole::ModalEntryDir
        } else {
            TextRole::ModalEntry
        };
        let label = if entry.is_dir {
            format!("{}/", entry.label)
        } else {
            entry.label.clone()
        };
        d.text(rect, role, label);
        y += row_h as i32;
    }

    if picker.entries.is_empty() {
        d.text(line(y), TextRole::ModalEntry, "no media here".to_string());
    }
}

/// A panel covering the middle of the window, snapped to whole rows so text
/// never straddles a row boundary.
fn centred(window: Rect, row_h: u32) -> Rect {
    let width = (window.width * 3 / 4).max(1);
    let height = ((window.height * 3 / 5) / row_h).max(3) * row_h;
    Rect {
        x: window.x + (window.width.saturating_sub(width) / 2) as i32,
        y: window.y + (window.height.saturating_sub(height) / 2) as i32,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_tile_the_window_without_overlapping() {
        let l = Layout::compute(800, 600, Metrics::default(), true, false);
        let heights = l.video.height
            + l.ruler.height
            + l.tracks.height
            + l.command.map_or(0, |c| c.height)
            + l.status.height;
        assert_eq!(heights, 600);
        assert_eq!(l.headers.height, l.tracks.height);
        assert_eq!(l.tracks.x, l.metrics.track_header_width as i32);
    }

    #[test]
    fn a_very_short_window_gives_up_timeline_height_not_correctness() {
        let l = Layout::compute(400, 10, Metrics::default(), true, false);
        assert_eq!(l.status.height, 10);
        assert_eq!(l.tracks.height, 0);
        assert_eq!(l.surface().rows, 1);
    }

    #[test]
    fn a_very_narrow_window_keeps_at_least_one_column() {
        let l = Layout::compute(1, 600, Metrics::default(), false, false);
        assert_eq!(l.surface().columns, 1);
        assert!(l.command.is_none());
    }

    #[test]
    fn surface_reports_one_column_per_pixel_and_one_row_per_lane() {
        let l = Layout::compute(800, 600, Metrics::default(), false, false);
        let s = l.surface();
        assert_eq!(s.columns, 800 - 80);
        assert_eq!(s.rows, (l.tracks.height / 40) as usize);
    }
}
