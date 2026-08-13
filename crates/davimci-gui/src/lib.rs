//! The primary GUI frontend.
//!
//! Everything here is a pure function of a [`davimci_app::ViewState`] plus
//! window size: the layout, the draw list, the key translation, and the state
//! of the `:` line, the media picker, and INSERT-mode subtitle editing. No
//! view logic lives here - the crate cannot even reach a `Timeline`, since it
//! does not depend on `davimci-core`'s timeline API for anything but ids.
//!
//! The `egui` shell behind the `window` feature only uploads what
//! [`paint::DrawList`] and the presenter already decided on; it never
//! reinterprets them. That is what lets the layout, painting and input tests
//! run with no display present.

#[cfg(feature = "window")]
pub mod egui_shell;
pub mod layout;
pub mod paint;
pub mod shell;

// The raw key alphabet is app state too: both frontends translate through
// the same table in `davimci_app::rawkey`.
pub use davimci_app::rawkey::{Modifiers, RawKey, translate};
pub use layout::{Layout, Metrics, VideoHeight, paint as paint_view};
pub use paint::{Chrome, DrawList, Fill, Paint, Rect, TextRole, VideoQuad};
// The modals are app state, not GUI state: the TUI opens the same ones.
pub use davimci_app::{
    Entry, MediaPicker, Numbers, PickerEvent, PickerIntent, SubtitleEdit, SubtitleEvent,
};
pub use shell::{Gui, GuiEvent};
