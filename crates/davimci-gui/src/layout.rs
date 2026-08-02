//! Window layout and timeline painting (plan.md Phase 9c).
//!
//! The window is one column: video pane on top, timeline below it, then the
//! status line and (when open) the command line. Every size here is derived,
//! never stored, so an extreme window size produces a small layout rather
//! than an inconsistent one.

use davimci_app::{Surface, ViewState};

use crate::paint::{Chrome, DrawList, Fill, Rect, TextRole, status_text};

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
    pub metrics: Metrics,
}

impl Layout {
    /// Lay out a window of `width` x `height` pixels.
    ///
    /// Panes are given away in priority order - status line, command line,
    /// ruler, video, timeline - so a window too short for everything loses
    /// the timeline's height rather than producing negative sizes.
    #[must_use]
    pub fn compute(width: u32, height: u32, metrics: Metrics, command_open: bool) -> Self {
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

    // Video pane: the presenter has already letterboxed, so the shell only
    // places the quad it was handed.
    d.rect(layout.video, Fill::Background);
    if let Some(q) = chrome.video {
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

    // Ruler.
    d.rect(layout.ruler, Fill::Ruler);
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

    // Track lanes.
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
            let (first, last) = clip.columns;
            let rect = Rect {
                x: layout.tracks.x.saturating_add(first as i32),
                y: y.saturating_add(1),
                width: last.saturating_sub(first).saturating_add(1),
                height: row_h.saturating_sub(2),
            };
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
            d.text(rect, TextRole::ClipLabel, clip.label.clone());
        }
    }

    // Selection band, drawn over the lanes it covers.
    if let Some(sel) = &view.selection
        && let Some((first, last)) = sel.columns
    {
        for (row, track) in view.tracks.iter().enumerate() {
            if !sel.tracks.contains(&track.id) {
                continue;
            }
            d.rect(
                Rect {
                    x: layout.tracks.x.saturating_add(first as i32),
                    y: layout.lane_y(row),
                    width: last.saturating_sub(first).saturating_add(1),
                    height: row_h,
                },
                Fill::Selection,
            );
        }
    }

    // Playhead: one pixel through the ruler and every lane.
    if let Some(col) = view.playhead.column {
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

    // Status and command lines.
    d.rect(layout.status, Fill::StatusLine);
    d.text(layout.status, TextRole::Status, status_text(view));
    if let (Some(rect), Some(line)) = (layout.command, view.command_line.as_ref()) {
        d.rect(rect, Fill::CommandLine);
        d.text(rect, TextRole::Command, format!(":{line}"));
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_tile_the_window_without_overlapping() {
        let l = Layout::compute(800, 600, Metrics::default(), true);
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
        let l = Layout::compute(400, 10, Metrics::default(), true);
        assert_eq!(l.status.height, 10);
        assert_eq!(l.tracks.height, 0);
        assert_eq!(l.surface().rows, 1);
    }

    #[test]
    fn a_very_narrow_window_keeps_at_least_one_column() {
        let l = Layout::compute(1, 600, Metrics::default(), false);
        assert_eq!(l.surface().columns, 1);
        assert!(l.command.is_none());
    }

    #[test]
    fn surface_reports_one_column_per_pixel_and_one_row_per_lane() {
        let l = Layout::compute(800, 600, Metrics::default(), false);
        let s = l.surface();
        assert_eq!(s.columns, 800 - 80);
        assert_eq!(s.rows, (l.tracks.height / 40) as usize);
    }
}
