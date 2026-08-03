//! The v1 event list (spec 9.8) and what dispatching one produces.

use mlua::{Lua, Table};

/// An editor event a user `autocmd` can hook.
///
/// The payload is deliberately plain data, not a live `Timeline` handle: a
/// handler observes what happened and asks for edits through `davimci.editor`
/// (which queues a [`crate::request::Request`]), so Lua never becomes a
/// second write path into the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PlayheadMoved { frame: u64, track: String },
    SplitPerformed { frame: u64, track: String },
    ClipDeleted { clip: u64, track: String },
    ClipInserted { clip: u64, track: String },
    ModeChanged { from: String, to: String },
    BeforeExport { preset: String, output: String },
    AfterExport { preset: String, output: String },
    ProjectLoaded { path: String },
}

impl Event {
    /// The name `davimci.autocmd.on` binds against.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::PlayheadMoved { .. } => "PlayheadMoved",
            Self::SplitPerformed { .. } => "SplitPerformed",
            Self::ClipDeleted { .. } => "ClipDeleted",
            Self::ClipInserted { .. } => "ClipInserted",
            Self::ModeChanged { .. } => "ModeChanged",
            Self::BeforeExport { .. } => "BeforeExport",
            Self::AfterExport { .. } => "AfterExport",
            Self::ProjectLoaded { .. } => "ProjectLoaded",
        }
    }

    /// Whether a handler may abort what is about to happen. Only
    /// `BeforeExport` is cancellable in v1 (plan.md Phase 7, Phase 8b).
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        matches!(self, Self::BeforeExport { .. })
    }

    pub(crate) fn to_table(&self, lua: &Lua) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("event", self.name())?;
        match self {
            Self::PlayheadMoved { frame, track } => {
                t.set("frame", *frame)?;
                t.set("track", track.as_str())?;
            }
            Self::SplitPerformed { frame, track } => {
                t.set("frame", *frame)?;
                t.set("track", track.as_str())?;
            }
            Self::ClipDeleted { clip, track } | Self::ClipInserted { clip, track } => {
                t.set("clip", *clip)?;
                t.set("track", track.as_str())?;
            }
            Self::ModeChanged { from, to } => {
                t.set("from", from.as_str())?;
                t.set("to", to.as_str())?;
            }
            Self::BeforeExport { preset, output } | Self::AfterExport { preset, output } => {
                t.set("preset", preset.as_str())?;
                t.set("output", output.as_str())?;
            }
            Self::ProjectLoaded { path } => {
                t.set("path", path.as_str())?;
            }
        }
        Ok(t)
    }
}

/// One handler that failed, and therefore is disabled for the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerFailure {
    pub id: u32,
    pub event: String,
    pub message: String,
}

/// What came back from dispatching one event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dispatch {
    /// Set when a handler refused a cancellable event; carries the sentence
    /// the caller must show and must not proceed past.
    pub cancelled: Option<String>,
    /// Handlers that threw. Each is disabled for the rest of the session.
    pub failures: Vec<HandlerFailure>,
    /// Edits the handlers asked for, in call order.
    pub requests: Vec<crate::request::Request>,
}

impl Dispatch {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.is_some()
    }
}
