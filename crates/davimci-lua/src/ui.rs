//! What `davimci.ui` asks for, as plain data.
//!
//! A panel is view state, and the view layer sits above this crate, so
//! nothing here is a `davimci-app` type: a request says what the plugin
//! wants and the host translates it. The same reason `davimci.editor` queues
//! an action instead of editing.

/// A panel a plugin opened, identified for the rest of the session.
pub type PanelHandle = u32;

/// Where a panel asks to be pinned. Placement is the host's decision; this
/// is only the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelAnchor {
    #[default]
    Center,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Playhead,
}

impl PanelAnchor {
    /// Every spelling a config may use, for the error that lists them.
    pub const NAMES: &'static str =
        "center, top-left, top-right, bottom-left, bottom-right, playhead";

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "center" | "centre" => Self::Center,
            "top-left" => Self::TopLeft,
            "top-right" => Self::TopRight,
            "bottom-left" => Self::BottomLeft,
            "bottom-right" => Self::BottomRight,
            "playhead" => Self::Playhead,
            _ => return None,
        })
    }
}

/// How a span of panel text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PanelRole {
    #[default]
    Normal,
    Key,
    Accent,
    Warning,
}

impl PanelRole {
    pub const NAMES: &'static str = "normal, key, accent, warning";

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelLine {
    pub spans: Vec<PanelSpan>,
}

/// What a panel is asked to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelContent {
    Lines(Vec<PanelLine>),
    Picture {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
    },
}

/// The panel as opened.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PanelSpec {
    pub title: Option<String>,
    pub anchor: PanelAnchor,
    pub columns: Option<u32>,
    pub rows: Option<u32>,
    pub z: i32,
    /// A focused panel takes the keyboard while it is open. Off by default:
    /// a panel that only reports must never eat a keystroke.
    pub focus: bool,
    /// The callback focused keys are handed to.
    pub on_key: Option<crate::registry::HandlerId>,
}

/// One thing to do to a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelRequest {
    Open {
        handle: PanelHandle,
        spec: Box<PanelSpec>,
    },
    SetContent {
        handle: PanelHandle,
        content: PanelContent,
    },
    Show(PanelHandle),
    Hide(PanelHandle),
    Close(PanelHandle),
}

impl PanelRequest {
    #[must_use]
    pub fn handle(&self) -> PanelHandle {
        match self {
            Self::Open { handle, .. }
            | Self::SetContent { handle, .. }
            | Self::Show(handle)
            | Self::Hide(handle)
            | Self::Close(handle) => *handle,
        }
    }
}
