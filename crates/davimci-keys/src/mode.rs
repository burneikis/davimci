//! Modes and visual selection state.

use davimci_core::{Frame, TrackId};
use davimci_motion::{Direction, TimeRange};

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

/// The active selection in `VISUAL` / `VISUAL-LINE` / `VISUAL-BLOCK`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualSelection {
    pub anchor: Anchor,
    pub active: Anchor,
    /// The set of tracks the selection covers. Block mode toggles members
    /// with `j`/`k` + a toggle key; the other visual modes always
    /// hold exactly the track the selection started on.
    pub tracks: Vec<TrackId>,
}

impl VisualSelection {
    fn new(at: Anchor) -> Self {
        Self {
            anchor: at,
            active: at,
            tracks: vec![at.track],
        }
    }

    /// `o`: swap the active end.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.anchor, &mut self.active);
    }

    #[must_use]
    pub fn range(&self) -> TimeRange {
        let a = self.anchor.frame;
        let b = self.active.frame;
        // Visual selections are inclusive of the frame under the active end,
        // matching vim's inclusive character-visual semantics.
        let end = Frame(a.max(b).get() + 1);
        TimeRange::new(a.min(b), end)
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

    fn extend(&mut self, to: Anchor, dir: Direction) {
        let _ = dir;
        self.active = to;
    }
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
    pub fn toggle_visual(&mut self, kind: Mode, at: Anchor) -> ModeChanged {
        debug_assert!(kind.is_visual());
        let from = self.mode;
        if self.mode == kind {
            self.mode = Mode::Normal;
            self.visual = None;
        } else {
            self.mode = kind;
            self.visual = Some(VisualSelection::new(at));
        }
        ModeChanged {
            from,
            to: self.mode,
        }
    }

    pub fn extend_visual(&mut self, to: Anchor, dir: Direction) {
        if let Some(v) = &mut self.visual {
            v.extend(to, dir);
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

    #[test]
    fn visual_toggles_off_on_the_same_key() {
        let mut m = ModeState::new();
        assert_eq!(m.mode(), Mode::Normal);
        let c = m.toggle_visual(Mode::Visual, a(0));
        assert_eq!(
            c,
            ModeChanged {
                from: Mode::Normal,
                to: Mode::Visual
            }
        );
        assert!(m.visual().is_some());
        let c = m.toggle_visual(Mode::Visual, a(0));
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
        m.toggle_visual(Mode::Visual, a(10));
        m.extend_visual(a(3), Direction::Backward);
        let Some(v) = m.visual() else {
            unreachable!("just entered visual");
        };
        let r = v.range();
        assert_eq!((r.start, r.end), (Frame(3), Frame(11)));
    }
}
