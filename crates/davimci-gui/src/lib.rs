//! The primary GUI frontend (plan.md Phase 9c).
//!
//! Everything here is a pure function of a [`davimci_app::ViewState`] plus
//! window size: the layout, the draw list, the key translation, and the state
//! of the `:` line, the media picker, and INSERT-mode subtitle editing. No
//! view logic lives here - the crate cannot even reach a `Timeline`, since it
//! does not depend on `davimci-core`'s timeline API for anything but ids.
//!
//! **Windowing status.** The `winit` + `wgpu` + `egui` shell that uploads
//! [`paint::DrawList`] and the presenter's RGBA surface to a real window is
//! not implemented yet; see README and plan.md Phase 9c. The split is
//! deliberate rather than cosmetic: the shell can only place pixels the model
//! below already decided on, so when it lands it must reproduce these draw
//! lists rather than reinterpret them, and the layout, painting and input
//! tests keep passing with no display present.

pub mod cmdline;
pub mod input;
pub mod layout;
pub mod paint;
pub mod picker;
pub mod shell;
pub mod subtitle;

pub use cmdline::{CommandLine, CommandLineEvent};
pub use input::{Modifiers, RawKey, translate};
pub use layout::{Layout, Metrics, paint as paint_view};
pub use paint::{Chrome, DrawList, Fill, Paint, Rect, TextRole, VideoQuad};
pub use picker::{Entry, MediaPicker, PickerEvent, PickerIntent};
pub use shell::{Gui, GuiEvent};
pub use subtitle::{SubtitleEdit, SubtitleEvent};
