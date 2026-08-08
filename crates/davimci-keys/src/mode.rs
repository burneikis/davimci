//! Modes and visual selection state.
//!
//! A selection is a time range across a set of tracks, held as two ends that
//! each cover a *unit* - a frame, a jump interval, or a whole clip, depending
//! on the mode and on `visualstart`. `docs/visual-mode.md` states the rule;
//! this module enforces it without a timeline, so the unit arrives already
//! resolved from [`crate::engine::Engine`].

use davimci_core::{Frame, TrackId};
use davimci_motion::TimeRange;

/// The editor's mode. A strict transition table lives in [`ModeState`]:
/// `Esc` from any state returns to `Normal`, and nothing else is reachable
/// except through an explicit `enter_*` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Visual,
    VisualLine,
    VisualBlock,
    Insert,
    Command,
}

impl Mode {
    /// The name a config spells this mode with, and the one a
    /// `ModeChanged` event carries. The same table `map()` parses,
    /// read the other way, so the two cannot drift.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Visual => "visual",
            Self::VisualLine => "visual-line",
            Self::VisualBlock => "visual-block",
            Self::Insert => "insert",
            Self::Command => "command",
        }
    }

    #[must_use]
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine | Mode::VisualBlock)
    }
}

/// Fired whenever [`ModeState`] changes mode, for Lua `autocmd`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeChanged {
    pub from: Mode,
    pub to: Mode,
}

/// A playhead-shaped position, independent of `davimci-motion`'s `Position` so
/// this crate does not need a live `Timeline` to track a selection anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub frame: Frame,
    pub track: TrackId,
}

/// What `v` and `<C-v>` cover at each end: `:set visualstart`.
///
/// `V` ignores it - its unit is always the clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VisualStart {
    /// One frame.
    #[default]
    Frame,
    /// The jump-point interval containing the frame.
    Jump,
}

impl VisualStart {
    pub const NAMES: &'static [&'static str] = &["frame", "jump"];

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "frame" => Some(Self::Frame),
            "jump" => Some(Self::Jump),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Jump => "jump",
        }
    }

    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Frame => "visual mode starts on one frame",
            Self::Jump => "visual mode starts on the jump interval under the cursor",
        }
    }
}

/// The active selection in `VISUAL` / `VISUAL-LINE` / `VISUAL-BLOCK`.
///
/// Both ends carry the unit they cover, and the selection is the union of the
/// two. That is what makes `v` a frame and `V` a clip without a second range
/// type: only the unit differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualSelection {
    pub anchor: Anchor,
    pub active: Anchor,
    /// The span the anchor end covers.
    pub anchor_span: TimeRange,
    /// The span the active end covers.
    pub active_span: TimeRange,
    /// The set of tracks the selection covers, in timeline order. `j`/`k`
    /// make it the contiguous span between the two ends' tracks; block mode
    /// may then punch holes in it with an explicit toggle.
    pub tracks: Vec<TrackId>,
}

impl VisualSelection {
    fn new(at: Anchor, span: TimeRange) -> Self {
        Self {
            anchor: at,
            active: at,
            anchor_span: span,
            active_span: span,
            tracks: vec![at.track],
        }
    }

    /// `o`: swap the active end. The units travel with the ends they belong
    /// to, or a swap would silently reshape the selection.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.anchor, &mut self.active);
        std::mem::swap(&mut self.anchor_span, &mut self.active_span);
    }

    #[must_use]
    pub fn range(&self) -> TimeRange {
        let a = self.anchor_span;
        let b = self.active_span;
        TimeRange::new(a.start.min(b.start), a.end.max(b.end))
    }

    pub fn toggle_track(&mut self, track: TrackId) {
        if let Some(i) = self.tracks.iter().position(|t| *t == track) {
            if self.tracks.len() > 1 {
                self.tracks.remove(i);
            }
        } else {
            self.tracks.push(track);
        }
    }

    fn extend(&mut self, to: Anchor, span: TimeRange, order: &[TrackId]) {
        let moved_track = to.track != self.active.track;
        self.active = to;
        self.active_span = span;
        if moved_track {
            self.tracks = tracks_between(order, self.anchor.track, to.track);
        }
    }
}

/// Every track from `a` to `b` inclusive, in timeline order.
///
/// A track the caller cannot place - one deleted between the anchor and the
/// motion - collapses to the end that is still there rather than emptying the
/// selection, because an empty track set is not a selection at all.
fn tracks_between(order: &[TrackId], a: TrackId, b: TrackId) -> Vec<TrackId> {
    let (Some(i), Some(j)) = (
        order.iter().position(|t| *t == a),
        order.iter().position(|t| *t == b),
    ) else {
        return vec![b];
    };
    order[i.min(j)..=i.max(j)].to_vec()
}

/// Mode plus whatever state that mode owns.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeState {
    mode: Mode,
    visual: Option<VisualSelection>,
}

impl Default for ModeState {
    fn default() -> Self {
        Self::new()
    }
}

impl ModeState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            mode: Mode::Normal,
            visual: None,
        }
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    #[must_use]
    pub fn visual(&self) -> Option<&VisualSelection> {
        self.visual.as_ref()
    }

    /// `v` / `V` / `Ctrl-v`: enters the given visual mode, or - vim-like -
    /// leaves visual mode if that variant is already active.
    pub fn toggle_visual(&mut self, kind: Mode, at: Anchor, span: TimeRange) -> ModeChanged {
        debug_assert!(kind.is_visual());
        let from = self.mode;
        if self.mode == kind {
            self.mode = Mode::Normal;
            self.visual = None;
        } else {
            self.mode = kind;
            self.visual = Some(VisualSelection::new(at, span));
        }
        ModeChanged {
            from,
            to: self.mode,
        }
    }

    /// Move the active end onto `to`, which covers `span`. `order` is the
    /// timeline's tracks, so a vertical motion can rebuild the track span.
    pub fn extend_visual(&mut self, to: Anchor, span: TimeRange, order: &[TrackId]) {
        if let Some(v) = &mut self.visual {
            v.extend(to, span, order);
        }
    }

    pub fn swap_visual_ends(&mut self) {
        if let Some(v) = &mut self.visual {
            v.swap();
        }
    }

    /// Replace the selection's track set (`it`/`at` in VISUAL).
    /// Ignored when empty: a selection always covers at least one track.
    pub fn set_visual_tracks(&mut self, tracks: Vec<TrackId>) {
        if let Some(v) = &mut self.visual
            && !tracks.is_empty()
        {
            v.tracks = tracks;
        }
    }

    pub fn toggle_visual_track(&mut self, track: TrackId) {
        if let Some(v) = &mut self.visual {
            v.toggle_track(track);
        }
    }

    /// Any transition that is not visual entry/exit or `Esc`, e.g. `c` +
    /// motion dropping into `INSERT`, or `:` entering `COMMAND`.
    pub fn enter(&mut self, to: Mode) -> ModeChanged {
        let from = self.mode;
        self.mode = to;
        if !to.is_visual() {
            self.visual = None;
        }
        ModeChanged { from, to }
    }

    /// `Esc`: every mode returns to `Normal`. This is the one transition
    /// guaranteed reachable from anywhere.
    pub fn escape(&mut self) -> ModeChanged {
        self.enter(Mode::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(f: u64) -> Anchor {
        Anchor {
            frame: Frame(f),
            track: TrackId(1),
        }
    }

    fn on(f: u64, track: u64) -> Anchor {
        Anchor {
            frame: Frame(f),
            track: TrackId(track),
        }
    }

    /// The one-frame unit `visualstart=frame` produces.
    fn frame_unit(at: Anchor) -> TimeRange {
        TimeRange::new(at.frame, Frame(at.frame.get() + 1))
    }

    const ORDER: &[TrackId] = &[TrackId(1), TrackId(2), TrackId(3), TrackId(4)];

    #[test]
    fn visual_toggles_off_on_the_same_key() {
        let mut m = ModeState::new();
        assert_eq!(m.mode(), Mode::Normal);
        let c = m.toggle_visual(Mode::Visual, a(0), frame_unit(a(0)));
        assert_eq!(
            c,
            ModeChanged {
                from: Mode::Normal,
                to: Mode::Visual
            }
        );
        assert!(m.visual().is_some());
        let c = m.toggle_visual(Mode::Visual, a(0), frame_unit(a(0)));
        assert_eq!(
            c,
            ModeChanged {
                from: Mode::Visual,
                to: Mode::Normal
            }
        );
        assert!(m.visual().is_none());
    }

    #[test]
    fn escape_always_returns_to_normal() {
        for start in [
            Mode::Visual,
            Mode::VisualLine,
            Mode::VisualBlock,
            Mode::Insert,
            Mode::Command,
        ] {
            let mut m = ModeState::new();
            m.enter(start);
            assert_eq!(m.mode(), start);
            m.escape();
            assert_eq!(m.mode(), Mode::Normal);
        }
    }

    #[test]
    fn a_selection_range_is_inclusive_and_normalised() {
        let mut m = ModeState::new();
        m.toggle_visual(Mode::Visual, a(10), frame_unit(a(10)));
        m.extend_visual(a(3), frame_unit(a(3)), ORDER);
        let Some(v) = m.visual() else {
            unreachable!("just entered visual");
        };
        let r = v.range();
        assert_eq!((r.start, r.end), (Frame(3), Frame(11)));
    }

    #[test]
    fn entering_visual_selects_only_the_unit_under_the_cursor() {
        let mut m = ModeState::new();
        m.toggle_visual(Mode::Visual, a(10), frame_unit(a(10)));
        let Some(v) = m.visual() else {
            unreachable!("just entered visual");
        };
        assert_eq!((v.range().start, v.range().end), (Frame(10), Frame(11)));
        assert_eq!(v.tracks, vec![TrackId(1)]);
    }

    #[test]
    fn a_selection_is_the_union_of_the_two_ends_units() {
        // `V`-shaped: each end covers a whole clip, so the selection covers
        // both clips whole even though neither end sits on a boundary.
        let mut m = ModeState::new();
        m.toggle_visual(Mode::VisualLine, a(10), TimeRange::new(Frame(0), Frame(50)));
        m.extend_visual(a(120), TimeRange::new(Frame(100), Frame(200)), ORDER);
        let Some(v) = m.visual() else {
            unreachable!("just entered visual");
        };
        assert_eq!((v.range().start, v.range().end), (Frame(0), Frame(200)));
    }

    #[test]
    fn a_vertical_motion_makes_the_track_set_the_span_between_the_ends() {
        let mut m = ModeState::new();
        let start = on(10, 2);
        m.toggle_visual(Mode::Visual, start, frame_unit(start));
        let down = on(10, 4);
        m.extend_visual(down, frame_unit(down), ORDER);
        let Some(v) = m.visual() else {
            unreachable!("just entered visual");
        };
        assert_eq!(v.tracks, vec![TrackId(2), TrackId(3), TrackId(4)]);
        // Back up: the span shrinks with the active end rather than growing.
        let up = on(10, 1);
        m.extend_visual(up, frame_unit(up), ORDER);
        let Some(v) = m.visual() else {
            unreachable!("still visual");
        };
        assert_eq!(v.tracks, vec![TrackId(1), TrackId(2)]);
    }

    #[test]
    fn a_horizontal_motion_leaves_the_track_set_alone() {
        let mut m = ModeState::new();
        let start = on(10, 2);
        m.toggle_visual(Mode::VisualBlock, start, frame_unit(start));
        m.toggle_visual_track(TrackId(4));
        let along = on(40, 2);
        m.extend_visual(along, frame_unit(along), ORDER);
        let Some(v) = m.visual() else {
            unreachable!("still visual");
        };
        assert_eq!(v.tracks, vec![TrackId(2), TrackId(4)]);
    }

    #[test]
    fn swapping_ends_carries_each_ends_unit_with_it() {
        let mut m = ModeState::new();
        m.toggle_visual(Mode::VisualLine, a(10), TimeRange::new(Frame(0), Frame(50)));
        m.extend_visual(a(120), TimeRange::new(Frame(100), Frame(200)), ORDER);
        m.swap_visual_ends();
        let Some(v) = m.visual() else {
            unreachable!("still visual");
        };
        assert_eq!((v.range().start, v.range().end), (Frame(0), Frame(200)));
        assert_eq!(v.anchor_span, TimeRange::new(Frame(100), Frame(200)));
    }
}
