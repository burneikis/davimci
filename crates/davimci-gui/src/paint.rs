//! The draw list a GUI shell rasterises (plan.md Phase 9c).
//!
//! Painting is split from windowing on purpose: turning a [`ViewState`] into
//! rectangles is deterministic, testable with no window and no GPU, and is
//! the part a rendering regression actually lives in. The shell that uploads
//! these to `egui`/`wgpu` adds nothing that could change *what* is drawn.

use davimci_app::{Severity, ViewState};

/// A rectangle in window pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x
            && y >= self.y
            && x < self.x.saturating_add(self.width as i32)
            && y < self.y.saturating_add(self.height as i32)
    }
}

/// What a rectangle represents. The shell picks colours from this; nothing in
/// the paint list is a colour, so a theme cannot change the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Background,
    Ruler,
    TrackLane,
    TrackLaneFocused,
    TrackHeader,
    Clip,
    ClipSelected,
    ClipOffline,
    ClipLinked,
    Selection,
    Playhead,
    TickMajor,
    TickMinor,
    StatusLine,
    CommandLine,
    Video,
}

/// Where text sits and what it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRole {
    TrackName,
    ClipLabel,
    Status,
    Command,
    Message(Severity),
    Timecode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paint {
    Rect {
        rect: Rect,
        fill: Fill,
    },
    Text {
        rect: Rect,
        role: TextRole,
        text: String,
    },
}

/// An ordered list of paint operations, back to front.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawList {
    ops: Vec<Paint>,
}

impl DrawList {
    pub fn rect(&mut self, rect: Rect, fill: Fill) {
        self.ops.push(Paint::Rect { rect, fill });
    }

    pub fn text(&mut self, rect: Rect, role: TextRole, text: impl Into<String>) {
        self.ops.push(Paint::Text {
            rect,
            role,
            text: text.into(),
        });
    }

    #[must_use]
    pub fn ops(&self) -> &[Paint] {
        &self.ops
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Every rectangle with a given fill - what the layout tests assert on.
    #[must_use]
    pub fn rects(&self, fill: Fill) -> Vec<Rect> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Paint::Rect { rect, fill: f } if *f == fill => Some(*rect),
                _ => None,
            })
            .collect()
    }

    #[must_use]
    pub fn texts(&self) -> Vec<&str> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Paint::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Anything the window knows that the [`ViewState`] does not: the video image
/// and where the `:` cursor is.
#[derive(Debug, Clone, Default)]
pub struct Chrome {
    /// Composited video from `davimci-present`, or `None` for a black pane.
    pub video: Option<VideoQuad>,
    /// Byte offset of the caret in the command line.
    pub command_cursor: usize,
}

/// The video pane's contents, already letterboxed by `davimci-present`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoQuad {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Timecode overlay, when the presenter described one.
    pub timecode: Option<TimecodeText>,
}

/// A timecode string is carried, never rasterised here: the shell draws it
/// with its own font (plan.md Phase 9b's rule about text stacks).
pub type TimecodeText = &'static str;

/// Debug helper: a one-line summary of a draw list, used by the layout tests
/// to keep failures readable.
#[must_use]
pub fn summarise(list: &DrawList) -> String {
    let mut counts = std::collections::BTreeMap::new();
    for op in list.ops() {
        let key = match op {
            Paint::Rect { fill, .. } => format!("{fill:?}"),
            Paint::Text { role, .. } => format!("{role:?}"),
        };
        *counts.entry(key).or_insert(0usize) += 1;
    }
    counts
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Convenience for shells: the status text a view wants on screen.
#[must_use]
pub fn status_text(view: &ViewState) -> String {
    let mut s = view.mode_line.clone();
    if let Some(job) = &view.job {
        s.push_str(&format!("  [{} {}%]", job.label, job.percent()));
    }
    if let Some(reg) = view.recording {
        s.push_str(&format!("  recording @{reg}"));
    }
    if let Some(msg) = &view.message {
        s.push_str("  ");
        s.push_str(&msg.text);
    }
    s
}
