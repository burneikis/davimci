//! Plugin panels: floating boxes a plugin owns and every frontend draws.
//!
//! A panel is view state, not a write path. A plugin says what it wants
//! shown; this module decides where it lands, in the same units the
//! [`crate::Surface`] reports, so the GUI and the TUI draw one placement
//! rather than two. Nothing here reaches the timeline, and a panel never
//! enters the undo log.

use std::collections::BTreeMap;
use std::sync::Arc;

/// A panel's identity, handed out by the plugin layer and unique per host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PanelId(pub u32);

impl PanelId {
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Most a panel may hold, so a runaway plugin costs a clipped panel rather
/// than the editor.
pub const MAX_LINES: usize = 200;
/// Most spans one line may hold.
pub const MAX_SPANS: usize = 64;
/// Most panels one host will place. Everything past this is dropped, oldest
/// panels keeping their place: a plugin cannot push the editor off screen.
pub const MAX_PANELS: usize = 16;
/// Widest picture a panel may carry, per side.
pub const MAX_PIXEL_SIDE: u32 = 4096;

/// Where a panel is pinned. Placement is clamped to the surface afterwards,
/// so an anchor is a request and never a guarantee of exact position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelAnchor {
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Follows the playhead column, which is what a contextual popup wants.
    Playhead,
}

/// How big a panel asks to be, in surface units. `None` means "as big as the
/// content needs", which is what a which-key panel wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelSize {
    pub columns: Option<u32>,
    pub rows: Option<u32>,
}

/// How a span is drawn. Frontends pick colours from this; the text is the
/// same everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelRole {
    #[default]
    Normal,
    /// A key, a mark, anything the user would type.
    Key,
    /// A heading or a group name.
    Accent,
    Warning,
}

impl PanelRole {
    /// The name Lua spells it with, and `dump` prints.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Key => "key",
            Self::Accent => "accent",
            Self::Warning => "warning",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "normal" => Self::Normal,
            "key" => Self::Key,
            "accent" => Self::Accent,
            "warning" => Self::Warning,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelSpan {
    pub text: String,
    pub role: PanelRole,
}

impl PanelSpan {
    pub fn new(text: impl Into<String>, role: PanelRole) -> Self {
        Self {
            text: sanitise(&text.into()),
            role,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelLine {
    pub spans: Vec<PanelSpan>,
}

impl PanelLine {
    #[must_use]
    pub fn width(&self) -> u32 {
        let chars: usize = self.spans.iter().map(|s| s.text.chars().count()).sum();
        u32::try_from(chars).unwrap_or(u32::MAX)
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// What a panel shows.
///
/// `Pixels` is the case a terminal cannot honour: the TUI degrades to the
/// title and a placeholder rather than failing, by the rule that a
/// recoverable shortfall keeps editing alive.
#[derive(Clone, PartialEq, Eq)]
pub enum PanelContent {
    Lines(Vec<PanelLine>),
    Pixels {
        width: u32,
        height: u32,
        /// `width * height * 4` bytes, shared rather than copied: a view is
        /// assembled every frame.
        rgba: Arc<Vec<u8>>,
    },
}

impl Default for PanelContent {
    fn default() -> Self {
        Self::Lines(Vec::new())
    }
}

impl std::fmt::Debug for PanelContent {
    /// Pixels are never printed: a `Debug` in a status line or a test failure
    /// has to stay readable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lines(lines) => f.debug_tuple("Lines").field(&lines.len()).finish(),
            Self::Pixels { width, height, .. } => f
                .debug_struct("Pixels")
                .field("width", width)
                .field("height", height)
                .finish_non_exhaustive(),
        }
    }
}

/// What a plugin asked for, before placement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelSpec {
    pub owner: String,
    pub title: Option<String>,
    pub anchor: PanelAnchor,
    pub size: PanelSize,
    pub z: i32,
    /// A focused panel owns the keyboard while it is open. Off by default,
    /// so a panel that only reports (which-key) can never eat a keystroke.
    pub focus: bool,
    /// The Lua callback focused keys are handed to, if any.
    pub on_key: Option<u32>,
}

/// A panel as the store holds it: what was asked for, plus what it shows and
/// whether it is currently on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub id: PanelId,
    pub spec: PanelSpec,
    pub content: PanelContent,
    pub visible: bool,
}

/// A rectangle in surface units: timeline columns across, rows down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelRect {
    pub column: u32,
    pub row: u32,
    pub columns: u32,
    pub rows: u32,
}

/// A placed panel: what a frontend draws, and all it needs to draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelView {
    pub id: PanelId,
    pub owner: String,
    pub title: Option<String>,
    pub rect: PanelRect,
    pub focus: bool,
    pub z: i32,
    pub content: PanelContent,
}

/// What a plugin asked to happen to a panel.
///
/// Applied to a [`PanelStore`] by the app; never a `Command`, because a panel
/// is not part of the project and must not reach the undo log.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelOp {
    Open { id: PanelId, spec: Box<PanelSpec> },
    SetContent { id: PanelId, content: PanelContent },
    Show(PanelId),
    Hide(PanelId),
    Close(PanelId),
}

impl PanelOp {
    #[must_use]
    pub fn id(&self) -> PanelId {
        match self {
            Self::Open { id, .. }
            | Self::SetContent { id, .. }
            | Self::Show(id)
            | Self::Hide(id)
            | Self::Close(id) => *id,
        }
    }
}

/// Every panel a host currently holds, in open order.
#[derive(Debug, Default, Clone)]
pub struct PanelStore {
    panels: BTreeMap<PanelId, Panel>,
    /// Open order, so `MAX_PANELS` drops the newest rather than an arbitrary
    /// one and z-ties break the way the user saw them appear.
    order: Vec<PanelId>,
}

impl PanelStore {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.panels.len()
    }

    #[must_use]
    pub fn get(&self, id: PanelId) -> Option<&Panel> {
        self.panels.get(&id)
    }

    /// The focused panel, if one is open: the newest one that asked for
    /// focus, so a panel opened over another gets the keyboard.
    #[must_use]
    pub fn focused(&self) -> Option<&Panel> {
        self.order
            .iter()
            .rev()
            .filter_map(|id| self.panels.get(id))
            .find(|p| p.visible && p.spec.focus)
    }

    /// Apply one op. Errors are user-facing sentences: a plugin asking for
    /// something impossible hears about it on the status line and the store
    /// is left as it was.
    pub fn apply(&mut self, op: PanelOp) -> Result<(), String> {
        match op {
            PanelOp::Open { id, spec } => {
                if !self.panels.contains_key(&id) && self.panels.len() >= MAX_PANELS {
                    return Err(format!(
                        "'{}' cannot open another panel: {MAX_PANELS} are already open.",
                        spec.owner
                    ));
                }
                if !self.panels.contains_key(&id) {
                    self.order.push(id);
                }
                let content = self
                    .panels
                    .remove(&id)
                    .map_or_else(PanelContent::default, |p| p.content);
                self.panels.insert(
                    id,
                    Panel {
                        id,
                        spec: *spec,
                        content,
                        visible: true,
                    },
                );
                Ok(())
            }
            PanelOp::SetContent { id, content } => {
                let content = clamp_content(content)?;
                let panel = self.expect(id)?;
                panel.content = content;
                Ok(())
            }
            PanelOp::Show(id) => {
                self.expect(id)?.visible = true;
                Ok(())
            }
            PanelOp::Hide(id) => {
                self.expect(id)?.visible = false;
                Ok(())
            }
            PanelOp::Close(id) => {
                if self.panels.remove(&id).is_none() {
                    return Err(missing(id));
                }
                self.order.retain(|o| *o != id);
                Ok(())
            }
        }
    }

    /// Close every panel a plugin left open - used when a runtime is
    /// replaced, so a reload cannot leave an orphan on screen.
    pub fn clear(&mut self) {
        self.panels.clear();
        self.order.clear();
    }

    /// Place every visible panel on a surface, back to front.
    ///
    /// A panel is clamped to the surface rather than refused: an oversized
    /// request reads as a big panel, never as a panel drawn off screen.
    #[must_use]
    pub fn place(&self, columns: u32, rows: u32, playhead_column: Option<u32>) -> Vec<PanelView> {
        let mut out: Vec<PanelView> = self
            .order
            .iter()
            .filter_map(|id| self.panels.get(id))
            .filter(|p| p.visible)
            .map(|p| PanelView {
                id: p.id,
                owner: p.spec.owner.clone(),
                title: p.spec.title.clone(),
                rect: place_one(p, columns, rows, playhead_column),
                focus: p.spec.focus,
                z: p.spec.z,
                content: p.content.clone(),
            })
            .collect();
        // A stable sort keeps open order inside one z level, so a panel does
        // not swap places with another between frames.
        out.sort_by_key(|p| p.z);
        out
    }

    fn expect(&mut self, id: PanelId) -> Result<&mut Panel, String> {
        self.panels.get_mut(&id).ok_or_else(|| missing(id))
    }
}

fn missing(id: PanelId) -> String {
    format!("Panel {} is not open.", id.get())
}

/// Cap what a panel holds and strip anything that would break a line.
fn clamp_content(content: PanelContent) -> Result<PanelContent, String> {
    match content {
        PanelContent::Lines(lines) => Ok(PanelContent::Lines(
            lines
                .into_iter()
                .take(MAX_LINES)
                .map(|line| PanelLine {
                    spans: line
                        .spans
                        .into_iter()
                        .take(MAX_SPANS)
                        .map(|s| PanelSpan {
                            text: sanitise(&s.text),
                            role: s.role,
                        })
                        .collect(),
                })
                .collect(),
        )),
        PanelContent::Pixels {
            width,
            height,
            rgba,
        } => {
            if width == 0 || height == 0 {
                return Err("A panel picture needs a width and a height.".to_string());
            }
            if width > MAX_PIXEL_SIDE || height > MAX_PIXEL_SIDE {
                return Err(format!(
                    "A panel picture may be at most {MAX_PIXEL_SIDE} pixels on a side."
                ));
            }
            let wanted = (width as usize) * (height as usize) * 4;
            if rgba.len() != wanted {
                return Err(format!(
                    "A {width}x{height} panel picture needs {wanted} bytes of RGBA, not {}.",
                    rgba.len()
                ));
            }
            Ok(PanelContent::Pixels {
                width,
                height,
                rgba,
            })
        }
    }
}

/// Drop control characters, which would otherwise move a terminal's cursor
/// out of the panel it was drawing.
fn sanitise(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The size a panel's content needs, before the surface caps it.
fn natural_size(panel: &Panel) -> (u32, u32) {
    let title = panel
        .spec
        .title
        .as_ref()
        .map_or(0, |t| u32::try_from(t.chars().count()).unwrap_or(u32::MAX));
    match &panel.content {
        PanelContent::Lines(lines) => {
            let widest = lines.iter().map(PanelLine::width).max().unwrap_or(0);
            let rows = u32::try_from(lines.len()).unwrap_or(u32::MAX);
            (widest.max(title), rows.max(1))
        }
        // A picture has no cells of its own; the panel states how many it
        // wants, and one row is the smallest thing a terminal can show.
        PanelContent::Pixels { .. } => (title.max(1), 1),
    }
}

fn place_one(panel: &Panel, columns: u32, rows: u32, playhead_column: Option<u32>) -> PanelRect {
    // Borders take a column each side and a row top and bottom; a panel with
    // no room for its border is drawn without one rather than not at all.
    const CHROME: u32 = 2;
    let (natural_w, natural_h) = natural_size(panel);
    let width = panel
        .spec
        .size
        .columns
        .unwrap_or(natural_w.saturating_add(CHROME))
        .clamp(1, columns.max(1));
    let height = panel
        .spec
        .size
        .rows
        .unwrap_or(natural_h.saturating_add(CHROME))
        .clamp(1, rows.max(1));
    let right = columns.saturating_sub(width);
    let bottom = rows.saturating_sub(height);
    let (column, row) = match panel.spec.anchor {
        PanelAnchor::Center => (right / 2, bottom / 2),
        PanelAnchor::TopLeft => (0, 0),
        PanelAnchor::TopRight => (right, 0),
        PanelAnchor::BottomLeft => (0, bottom),
        PanelAnchor::BottomRight => (right, bottom),
        PanelAnchor::Playhead => (playhead_column.unwrap_or(0).min(right), bottom),
    };
    PanelRect {
        column: column.min(right),
        row: row.min(bottom),
        columns: width,
        rows: height,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[allow(
        clippy::unnecessary_box_returns,
        reason = "a panel op carries its spec boxed"
    )]
    fn spec(anchor: PanelAnchor) -> Box<PanelSpec> {
        Box::new(PanelSpec {
            owner: "test".into(),
            anchor,
            ..PanelSpec::default()
        })
    }

    fn lines(n: usize, width: usize) -> PanelContent {
        PanelContent::Lines(
            (0..n)
                .map(|_| PanelLine {
                    spans: vec![PanelSpan::new("x".repeat(width), PanelRole::Normal)],
                })
                .collect(),
        )
    }

    fn store_with(anchor: PanelAnchor, content: PanelContent) -> PanelStore {
        let mut store = PanelStore::default();
        let id = PanelId(1);
        store
            .apply(PanelOp::Open {
                id,
                spec: spec(anchor),
            })
            .unwrap();
        store.apply(PanelOp::SetContent { id, content }).unwrap();
        store
    }

    #[test]
    fn a_panel_is_placed_inside_the_surface_at_any_size() {
        for anchor in [
            PanelAnchor::Center,
            PanelAnchor::TopLeft,
            PanelAnchor::TopRight,
            PanelAnchor::BottomLeft,
            PanelAnchor::BottomRight,
            PanelAnchor::Playhead,
        ] {
            for (cols, rows) in [(1u32, 1u32), (10, 3), (80, 24), (3, 100)] {
                for (n, w) in [(0usize, 0usize), (1, 5), (50, 200)] {
                    let store = store_with(anchor, lines(n, w));
                    for view in store.place(cols, rows, Some(cols * 2)) {
                        let r = view.rect;
                        assert!(r.columns >= 1 && r.rows >= 1, "{anchor:?} {cols}x{rows}");
                        assert!(
                            r.column + r.columns <= cols && r.row + r.rows <= rows,
                            "{anchor:?} {cols}x{rows} placed {r:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn panels_are_drawn_in_z_order_then_open_order() {
        let mut store = PanelStore::default();
        for (i, z) in [(1u32, 5i32), (2, -1), (3, 5)] {
            store
                .apply(PanelOp::Open {
                    id: PanelId(i),
                    spec: Box::new(PanelSpec {
                        z,
                        ..PanelSpec::default()
                    }),
                })
                .unwrap();
        }
        let ids: Vec<u32> = store
            .place(80, 24, None)
            .iter()
            .map(|p| p.id.get())
            .collect();
        assert_eq!(ids, vec![2, 1, 3]);
    }

    #[test]
    fn content_is_capped_and_control_characters_are_stripped() {
        let mut store = PanelStore::default();
        let id = PanelId(1);
        store
            .apply(PanelOp::Open {
                id,
                spec: spec(PanelAnchor::Center),
            })
            .unwrap();
        let mut long: Vec<PanelLine> = (0..MAX_LINES + 10).map(|_| PanelLine::default()).collect();
        long[0].spans = vec![PanelSpan::new("a\nb\tc", PanelRole::Key)];
        store
            .apply(PanelOp::SetContent {
                id,
                content: PanelContent::Lines(long),
            })
            .unwrap();
        let PanelContent::Lines(kept) = &store.get(id).unwrap().content else {
            panic!("lines");
        };
        assert_eq!(kept.len(), MAX_LINES);
        assert_eq!(kept[0].text(), "a b c");
    }

    #[test]
    fn a_malformed_picture_is_refused_with_a_sentence() {
        let mut store = PanelStore::default();
        let id = PanelId(1);
        store
            .apply(PanelOp::Open {
                id,
                spec: spec(PanelAnchor::Center),
            })
            .unwrap();
        let err = store
            .apply(PanelOp::SetContent {
                id,
                content: PanelContent::Pixels {
                    width: 2,
                    height: 2,
                    rgba: Arc::new(vec![0; 3]),
                },
            })
            .unwrap_err();
        assert!(err.ends_with('.'), "{err}");
        assert!(err.contains("RGBA"), "{err}");
    }

    #[test]
    fn operating_on_a_closed_panel_is_a_user_error() {
        let mut store = PanelStore::default();
        assert!(store.apply(PanelOp::Show(PanelId(9))).is_err());
        assert!(store.apply(PanelOp::Close(PanelId(9))).is_err());
    }

    #[test]
    fn the_panel_count_is_capped_per_host() {
        let mut store = PanelStore::default();
        for i in 0..MAX_PANELS {
            store
                .apply(PanelOp::Open {
                    id: PanelId(u32::try_from(i).unwrap()),
                    spec: spec(PanelAnchor::Center),
                })
                .unwrap();
        }
        assert!(
            store
                .apply(PanelOp::Open {
                    id: PanelId(999),
                    spec: spec(PanelAnchor::Center),
                })
                .is_err()
        );
        assert_eq!(store.len(), MAX_PANELS);
    }

    #[test]
    fn focus_is_the_newest_visible_panel_that_asked_for_it() {
        let mut store = PanelStore::default();
        let focused = |focus: bool| {
            Box::new(PanelSpec {
                focus,
                ..PanelSpec::default()
            })
        };
        store
            .apply(PanelOp::Open {
                id: PanelId(1),
                spec: focused(true),
            })
            .unwrap();
        store
            .apply(PanelOp::Open {
                id: PanelId(2),
                spec: focused(false),
            })
            .unwrap();
        assert_eq!(store.focused().map(|p| p.id), Some(PanelId(1)));
        store
            .apply(PanelOp::Open {
                id: PanelId(3),
                spec: focused(true),
            })
            .unwrap();
        assert_eq!(store.focused().map(|p| p.id), Some(PanelId(3)));
        store.apply(PanelOp::Hide(PanelId(3))).unwrap();
        assert_eq!(store.focused().map(|p| p.id), Some(PanelId(1)));
    }
}
