//! The draw list a GUI shell rasterises.
//!
//! Painting is split from windowing on purpose: turning a [`ViewState`] into
//! rectangles is deterministic, testable with no window and no GPU, and is
//! the part a rendering regression actually lives in. The shell that uploads
//! these to `egui`/`wgpu` adds nothing that could change *what* is drawn.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "a draw list is integer pixel arithmetic on window-sized values"
)]

use std::fmt::Write as _;

use davimci_app::{Severity, Thumbnail, ViewState};

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
    /// The header of the track the cursor is on.
    TrackHeaderFocused,
    Clip,
    ClipSelected,
    ClipOffline,
    /// A clip in a link group: the clip colour, a shade darker, so grouping
    /// reads without taking the colour that says what the clip is.
    ClipGrouped,
    Selection,
    /// One column of an audio lane's envelope.
    Waveform,
    Playhead,
    /// The playhead on the focused lane: where an edit would actually land.
    Cursor,
    TickMajor,
    TickMinor,
    StatusLine,
    CommandLine,
    /// The `:` line's caret.
    Caret,
    Video,
    /// The media picker's panel, drawn over everything else.
    ModalBackground,
    /// The row under the picker's cursor.
    ModalSelected,
}

/// Where text sits and what it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRole {
    TrackName,
    ClipLabel,
    Status,
    Command,
    /// Suggestions for the word being typed on the `:` line.
    Completion,
    Message(Severity),
    Timecode,
    /// A jump point's distance from the playhead, on the ruler.
    RulerNumber,
    /// The picker's title, e.g. "insert media".
    ModalTitle,
    /// What the user has typed to filter the list.
    ModalQuery,
    ModalEntry,
    ModalEntryDir,
    ModalEntrySelected,
}

impl Paint {
    /// True for anything belonging to a modal overlay.
    ///
    ///
    /// The video is a texture the shell draws between the world and the
    /// overlays, so a modal has to be identifiable to stay on top of it -
    /// otherwise the picker is painted and then covered by the video.
    #[must_use]
    pub fn is_modal(&self) -> bool {
        match self {
            Self::Rect { fill, .. } => {
                matches!(fill, Fill::ModalBackground | Fill::ModalSelected)
            }
            Self::Image { .. } => false,
            Self::Text { role, .. } => matches!(
                role,
                TextRole::ModalTitle
                    | TextRole::ModalQuery
                    | TextRole::ModalEntry
                    | TextRole::ModalEntryDir
                    | TextRole::ModalEntrySelected
            ),
        }
    }
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
    /// A clip's thumbnail, drawn inside its lane. Carried by handle rather
    /// than by pixels: the shell caches one texture per clip, and a draw
    /// list is built every frame.
    Image {
        rect: Rect,
        clip: davimci_core::ClipId,
        image: Thumbnail,
        /// Width of a whole tile. The last tile of a filmstrip is cut off at
        /// the clip's edge, and it must be *cropped* rather than squashed -
        /// so the shell shows `rect.width / tile` of the picture.
        tile: u32,
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

    pub fn image(&mut self, rect: Rect, clip: davimci_core::ClipId, image: Thumbnail, tile: u32) {
        self.ops.push(Paint::Image {
            rect,
            clip,
            image,
            tile,
        });
    }

    /// Every thumbnail in the list, in draw order - what a shell uploads.
    #[must_use]
    pub fn images(&self) -> Vec<(Rect, davimci_core::ClipId, &Thumbnail)> {
        self.ops
            .iter()
            .filter_map(|op| match op {
                Paint::Image {
                    rect, clip, image, ..
                } => Some((*rect, *clip, image)),
                _ => None,
            })
            .collect()
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
    /// The open media picker, if there is one. A modal the shell forgot to
    /// paint is a modal that silently eats every key, so this is not
    /// optional decoration - it is how `i`/`a`/`r` are usable at all.
    pub picker: Option<PickerView>,
    /// `:set numbers`: what the ruler labels its jump points with.
    pub numbers: davimci_app::Numbers,
}

/// Everything the painter needs to draw the media picker. Built by the
/// shell from its `MediaPicker`, so `paint` stays free of modal state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PickerView {
    pub title: String,
    pub query: String,
    /// Visible rows, already filtered.
    pub entries: Vec<PickerRow>,
    /// Index into `entries` of the highlighted row.
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRow {
    pub label: String,
    pub is_dir: bool,
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
/// with its own font.
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
            Paint::Image { .. } => "Thumbnail".to_string(),
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
        let _ = write!(s, "  [{} {}%]", job.label, job.percent());
    }
    if let Some(reg) = view.recording {
        let _ = write!(s, "  recording @{reg}");
    }
    if let Some(msg) = &view.message {
        s.push_str("  ");
        s.push_str(&msg.text);
    }
    s
}
