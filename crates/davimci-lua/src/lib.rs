//! Lua configuration and plugin API.
//!
//! Lua may ask, never write. Every `davimci.*` call either registers
//! something (a keymap, a motion, a text object, an export preset, an event
//! handler) or queues a [`Request`] for the host to run through the command
//! layer, so undo, `.`-repeat, and macros stay authoritative even when a
//! plugin drives the edit.
//!
//! A user callback that throws is disabled for the session, its notice goes
//! to the status line, and editing continues. Nothing here is fatal.
//!
//! ```no_run
//! use davimci_lua::{ConfigPaths, Runtime};
//!
//! let rt = Runtime::new()?;
//! if let Some(paths) = ConfigPaths::from_env() {
//!     for notice in rt.load_config(&paths) {
//!         eprintln!("{notice}");
//!     }
//! }
//! # Ok::<(), davimci_lua::LuaError>(())
//! ```

mod api;
pub mod config;
pub mod error;
pub mod event;
pub mod loader;
pub mod motion;
pub mod pack;
pub mod preset;
pub mod registry;
pub mod request;
pub mod runtime;
pub mod ui;

#[cfg(test)]
mod tests;

pub use api::EVENTS;
pub use config::TimelineConfig;
pub use error::LuaError;
pub use event::{Continuation, Dispatch, Event, HandlerFailure};
pub use loader::{ConfigPaths, DenyAll, Trust, TrustPrompt};
pub use motion::{MotionAnswer, MotionEnv, Sample, TrackData};
pub use pack::{API_VERSION, ApiRange, Manifest, Plugin, Provides, Source, Version};
pub use preset::{ExportPreset, SubtitleSelection, TrackSelection};
pub use registry::{HandlerId, KeyBinding, Rhs, TransitionDef};
pub use request::{OptValue, Opts, ProxySetup, Request, parse_editor_command};
pub use runtime::{ClipInfo, ObjectForm, Runtime, Sandbox};
pub use ui::{
    PanelAnchor, PanelContent, PanelHandle, PanelLine, PanelRequest, PanelRole, PanelSpan,
    PanelSpec,
};
