//! The optional terminal frontend.
//!
//! It renders the same [`davimci_app::ViewState`] the GUI does, routes modal
//! input through the same [`davimci_app::Modals`], and translates terminal
//! keys into the same `davimci-keys` tokens. No view logic lives here: a
//! divergence between this frontend and the window is a bug in one of them,
//! never a difference of opinion, and the parity test says so.
//!
//! What a terminal cannot do, and does not pretend to: no in-video overlays,
//! no properties panel, no filmstrips, and a timeline resolution of one cell
//! per column.

pub mod input;
pub mod render;
pub mod shell;
pub mod terminal;

pub use input::{Modifiers, TermKey, translate};
pub use render::{GUTTER, Overlay, lines, plain, surface};
pub use shell::{TermEvent, Tui};
pub use terminal::Terminal;
