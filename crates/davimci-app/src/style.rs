//! How the timeline draws the joins between clips.
//!
//! A cut and a gap are the two facts a timeline must not hide, and both are
//! invisible when abutting clips are painted edge to edge. The choice lives
//! here so the GUI and the TUI make it once, in the view, rather than each
//! inventing its own separator.

/// How the seam between two abutting clips is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EdgeStyle {
    /// Nothing: abutting clips run together, as they did before the setting
    /// existed.
    Off,
    /// A separator drawn *over* the first column of the later clip. Costs no
    /// width, so a clip one column wide never disappears.
    #[default]
    Line,
    /// The clips are pulled apart by [`TimelineStyle::gap`] on each abutting
    /// side, leaving lane background between them.
    Inset,
}

impl EdgeStyle {
    pub const NAMES: &'static [&'static str] = &["off", "line", "inset"];

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "off" | "none" => Some(Self::Off),
            "line" => Some(Self::Line),
            "inset" => Some(Self::Inset),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Line => "line",
            Self::Inset => "inset",
        }
    }
}

/// The timeline's separation settings, as one value both frontends read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineStyle {
    pub edges: EdgeStyle,
    /// How far [`EdgeStyle::Inset`] pulls a clip back from an abutting
    /// neighbour, in columns: GUI pixels, terminal cells. The TUI cannot
    /// draw a fraction of a cell, so it insets only when this is at least
    /// one column and the clip has a column to spare.
    pub gap: u32,
}

impl Default for TimelineStyle {
    fn default() -> Self {
        Self {
            edges: EdgeStyle::default(),
            gap: 1,
        }
    }
}

/// The widest inset accepted, so a mistyped `:set timeline.gap` cannot erase
/// the clips it is meant to separate.
pub const MAX_GAP: u32 = 32;
