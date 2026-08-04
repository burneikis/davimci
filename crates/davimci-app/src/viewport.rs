//! Viewport arithmetic.
//!
//! The viewport is the only thing that converts between timeline frames and
//! screen columns, and it is deliberately frontend-agnostic: a "column" is a
//! GUI pixel or a TUI cell, whichever the caller measures in. Both frontends
//! therefore scroll, zoom, and follow the playhead identically, which is what
//! the cross-frontend parity test relies on.

use davimci_core::Frame;
// Frames-per-column lives in `davimci-motion` beside `Zoom`, because the
// jump-point set is defined in on-screen density; the viewport
// re-exports it as the frontends' entry point.
use davimci_motion::Zoom;
pub use davimci_motion::{BASE_FRAMES_PER_COLUMN, frames_per_column};

/// Columns of slack kept between the playhead and the viewport edge when
/// scroll-follow kicks in, so `l` near the edge does not re-scroll on every
/// press.
pub const FOLLOW_MARGIN_COLUMNS: u32 = 4;

/// Horizontal and vertical scroll state over a timeline.
///
/// Invariants, restored by every mutator and asserted by the property tests:
/// - `start` is never past the timeline's duration;
/// - after [`Viewport::follow_playhead`] the playhead is inside the visible
///   frame range, and the focused track index inside the visible track range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    zoom: Zoom,
    start: Frame,
    /// Visible width, in columns. Zero-width viewports are legal (a frontend
    /// may be mid-resize) and behave as one column.
    columns: u32,
    top_track: usize,
    rows: usize,
}

impl Default for Viewport {
    fn default() -> Self {
        Self::new(80, 4)
    }
}

impl Viewport {
    #[must_use]
    pub fn new(columns: u32, rows: usize) -> Self {
        Self {
            zoom: Zoom::default(),
            start: Frame::ZERO,
            columns,
            top_track: 0,
            rows,
        }
    }

    #[must_use]
    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    #[must_use]
    pub fn start(&self) -> Frame {
        self.start
    }

    #[must_use]
    pub fn columns(&self) -> u32 {
        self.columns.max(1)
    }

    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows.max(1)
    }

    #[must_use]
    pub fn top_track(&self) -> usize {
        self.top_track
    }

    #[must_use]
    pub fn frames_per_column(&self) -> u64 {
        frames_per_column(self.zoom)
    }

    /// Number of frames the viewport shows. Always at least one.
    #[must_use]
    pub fn span(&self) -> Frame {
        Frame(
            self.frames_per_column()
                .saturating_mul(u64::from(self.columns()))
                .max(1),
        )
    }

    /// Visible frame range, half-open: `[start, start + span)`.
    #[must_use]
    pub fn visible_range(&self) -> (Frame, Frame) {
        (self.start, self.start.saturating_add(self.span()))
    }

    #[must_use]
    pub fn contains(&self, frame: Frame) -> bool {
        let (a, b) = self.visible_range();
        frame >= a && frame < b
    }

    /// Visible track index range, half-open.
    #[must_use]
    pub fn visible_tracks(&self) -> (usize, usize) {
        (self.top_track, self.top_track.saturating_add(self.rows()))
    }

    pub fn resize(&mut self, columns: u32, rows: usize) {
        self.columns = columns;
        self.rows = rows;
    }

    /// Column a frame lands in, or `None` when it is off-screen.
    #[must_use]
    pub fn column_of(&self, frame: Frame) -> Option<u32> {
        if !self.contains(frame) {
            return None;
        }
        let delta = frame.get().saturating_sub(self.start.get());
        u32::try_from(delta / self.frames_per_column()).ok()
    }

    /// Column a frame lands in, counting off the left edge - negative for a
    /// frame scrolled past. Needed where a layout is anchored to something
    /// off screen, such as a clip's filmstrip.
    #[must_use]
    pub fn column_of_unclamped(&self, frame: Frame) -> i64 {
        let fpc = self.frames_per_column() as i64;
        let delta = frame.get() as i64 - self.start.get() as i64;
        delta.div_euclid(fpc.max(1))
    }

    /// First frame shown in a column that may be off the left edge. Clamped
    /// at zero, since there are no frames before the timeline starts.
    #[must_use]
    pub fn frame_at_column_signed(&self, column: i64) -> Frame {
        let fpc = self.frames_per_column() as i64;
        let at = self.start.get() as i64 + column.saturating_mul(fpc);
        Frame(at.max(0) as u64)
    }

    /// First frame shown in `column`. The inverse of [`Viewport::column_of`]
    /// up to the frames-per-column quantum, which is what makes a click land
    /// on a frame.
    #[must_use]
    pub fn frame_at_column(&self, column: u32) -> Frame {
        self.start.saturating_add(Frame(
            self.frames_per_column().saturating_mul(u64::from(column)),
        ))
    }

    /// Scroll so `frame` is visible, moving the least distance that leaves
    /// [`FOLLOW_MARGIN_COLUMNS`] of slack, then clamp to the timeline.
    pub fn follow_playhead(&mut self, frame: Frame, duration: Frame) {
        let fpc = self.frames_per_column();
        let margin =
            Frame(fpc.saturating_mul(u64::from(FOLLOW_MARGIN_COLUMNS.min(self.columns() / 2))));
        let span = self.span();

        if frame < self.start.saturating_add(margin) {
            self.start = frame.saturating_sub(margin);
        } else {
            let right_limit = self.start.saturating_add(span).saturating_sub(margin);
            if frame >= right_limit {
                let want = frame.saturating_add(margin).saturating_sub(span);
                self.start = Frame(want.get().saturating_add(fpc.saturating_sub(1)) / fpc * fpc);
            }
        }
        self.clamp(frame, duration);
    }

    /// Scroll vertically so `index` is visible.
    pub fn follow_track(&mut self, index: usize, track_count: usize) {
        let rows = self.rows();
        if index < self.top_track {
            self.top_track = index;
        } else if index >= self.top_track.saturating_add(rows) {
            self.top_track = index.saturating_sub(rows - 1);
        }
        let max_top = track_count.saturating_sub(rows);
        self.top_track = self.top_track.min(max_top);
    }

    /// Zoom anchored on the playhead: the playhead keeps its column, so
    /// zooming never moves the thing the user is looking at.
    pub fn zoom_in(&mut self, playhead: Frame, duration: Frame) {
        self.set_zoom(self.zoom.zoom_in(), playhead, duration);
    }

    pub fn zoom_out(&mut self, playhead: Frame, duration: Frame) {
        self.set_zoom(self.zoom.zoom_out(), playhead, duration);
    }

    pub fn set_zoom(&mut self, zoom: Zoom, playhead: Frame, duration: Frame) {
        let old_col = u64::from(self.column_of(playhead).unwrap_or(self.columns() / 2));
        self.zoom = zoom;
        let offset = self.frames_per_column().saturating_mul(old_col);
        self.start = playhead.saturating_sub(Frame(offset));
        self.clamp(playhead, duration);
    }

    /// Zoom to show the whole of `duration` at once, scrolled to the start.
    ///
    /// Picks the finest level whose span still covers `duration`, so an
    /// imported clip fills the width instead of a few columns. An empty
    /// timeline fits at [`Zoom::MAX`]; a timeline longer than the widest
    /// span pins to [`Zoom::OUT`] and is simply clipped.
    pub fn fit(&mut self, duration: Frame) {
        let columns = u64::from(self.columns());
        let mut best = Zoom::OUT;
        for level in 0..=Zoom::MAX.level() {
            let zoom = Zoom::new(level);
            let span = frames_per_column(zoom).saturating_mul(columns);
            if span >= duration.get() {
                best = zoom;
            } else {
                break;
            }
        }
        self.zoom = best;
        self.start = Frame::ZERO;
    }

    /// Horizontal scroll by whole columns, positive is later in time.
    pub fn scroll_columns(&mut self, delta: i64, playhead: Frame, duration: Frame) {
        let step = self
            .frames_per_column()
            .saturating_mul(delta.unsigned_abs());
        self.start = if delta >= 0 {
            self.start.saturating_add(Frame(step))
        } else {
            self.start.saturating_sub(Frame(step))
        };
        self.clamp_bounds(duration);
        let _ = playhead;
    }

    /// Restore the bounds invariant, then re-guarantee the playhead is
    /// visible - following the playhead outranks staying inside the timeline,
    /// because the playhead may legally sit at `duration`.
    fn clamp(&mut self, playhead: Frame, duration: Frame) {
        self.clamp_bounds(duration);
        if !self.contains(playhead) {
            let span = self.span();
            self.start = if playhead < self.start {
                playhead
            } else {
                playhead.saturating_sub(span).saturating_add(Frame(1))
            };
        }
    }

    fn clamp_bounds(&mut self, duration: Frame) {
        if self.start > duration {
            self.start = duration;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        let mut v = Viewport::new(100, 3);
        v.set_zoom(Zoom::MAX, Frame::ZERO, Frame(10_000));
        v
    }

    #[test]
    fn frames_per_column_halves_per_level_and_bottoms_out_at_one() {
        assert_eq!(frames_per_column(Zoom::OUT), BASE_FRAMES_PER_COLUMN);
        assert_eq!(frames_per_column(Zoom::new(1)), BASE_FRAMES_PER_COLUMN / 2);
        assert_eq!(frames_per_column(Zoom::MAX), 1);
    }

    #[test]
    fn column_round_trips_within_the_quantum() {
        let mut v = Viewport::new(50, 2);
        v.set_zoom(Zoom::new(10), Frame::ZERO, Frame(100_000));
        let fpc = v.frames_per_column();
        let f = v.frame_at_column(7);
        assert_eq!(v.column_of(f), Some(7));
        assert_eq!(v.column_of(Frame(f.get() + fpc - 1)), Some(7));
    }

    #[test]
    fn follow_keeps_playhead_visible_moving_right() {
        let mut v = vp();
        v.follow_playhead(Frame(500), Frame(10_000));
        assert!(v.contains(Frame(500)));
        assert!(v.start().get() > 0);
    }

    #[test]
    fn follow_keeps_playhead_visible_moving_left() {
        let mut v = vp();
        v.follow_playhead(Frame(5_000), Frame(10_000));
        v.follow_playhead(Frame(10), Frame(10_000));
        assert!(v.contains(Frame(10)));
    }

    #[test]
    fn zoom_anchors_on_the_playhead_column() {
        let mut v = Viewport::new(64, 3);
        v.set_zoom(Zoom::new(8), Frame(4_000), Frame(100_000));
        v.follow_playhead(Frame(4_000), Frame(100_000));
        let before = v.column_of(Frame(4_000)).unwrap();
        v.zoom_in(Frame(4_000), Frame(100_000));
        assert_eq!(v.column_of(Frame(4_000)), Some(before));
        v.zoom_out(Frame(4_000), Frame(100_000));
        assert_eq!(v.column_of(Frame(4_000)), Some(before));
    }

    #[test]
    fn track_follow_scrolls_only_as_far_as_needed() {
        let mut v = Viewport::new(10, 3);
        v.follow_track(5, 8);
        assert_eq!(v.visible_tracks(), (3, 6));
        v.follow_track(1, 8);
        assert_eq!(v.visible_tracks(), (1, 4));
        v.follow_track(7, 8);
        assert_eq!(v.top_track(), 5);
    }

    #[test]
    fn track_follow_never_scrolls_past_the_last_track() {
        let mut v = Viewport::new(10, 5);
        v.follow_track(1, 2);
        assert_eq!(v.top_track(), 0);
    }

    #[test]
    fn scroll_is_clamped_to_the_timeline() {
        let mut v = vp();
        v.scroll_columns(10_000, Frame::ZERO, Frame(1_000));
        assert!(v.start() <= Frame(1_000));
        v.scroll_columns(-10_000, Frame::ZERO, Frame(1_000));
        assert_eq!(v.start(), Frame::ZERO);
    }

    #[test]
    fn fit_picks_the_finest_zoom_that_still_shows_everything() {
        let mut v = Viewport::new(100, 3);
        v.fit(Frame(400));
        assert_eq!(v.start(), Frame::ZERO);
        assert!(v.span() >= Frame(400));
        // One level finer would no longer fit.
        assert!(
            frames_per_column(v.zoom().zoom_in()) * 100 < 400 || v.zoom() == Zoom::MAX,
            "fit left a level of slack: {:?}",
            v.zoom()
        );
    }

    #[test]
    fn fit_pins_to_zoom_out_for_a_timeline_wider_than_any_span() {
        let mut v = Viewport::new(10, 3);
        v.fit(Frame(u64::MAX));
        assert_eq!(v.zoom(), Zoom::OUT);
        assert_eq!(v.start(), Frame::ZERO);
    }

    #[test]
    fn fit_on_an_empty_timeline_is_max_zoom() {
        let mut v = Viewport::new(80, 3);
        v.fit(Frame::ZERO);
        assert_eq!(v.zoom(), Zoom::MAX);
    }

    #[test]
    fn zero_width_viewport_does_not_divide_by_zero() {
        let mut v = Viewport::new(0, 0);
        v.follow_playhead(Frame(10), Frame(10));
        assert!(v.contains(Frame(10)));
        assert_eq!(v.rows(), 1);
    }
}
