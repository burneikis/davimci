//! The MLT render backend.
//!
//! This is the only crate in the workspace allowed to know that MLT exists.
//! It has four layers, deliberately separated by how testable they are:
//!
//! - [`projection`] turns a `Timeline` into the shape the graph must have -
//!   pure data, no MLT, no I/O;
//! - [`xml`] serialises that shape, giving golden tests that catch
//!   ripple/compositing regressions without rendering a frame;
//! - [`patch`] diffs two projections so an edit becomes playlist mutations
//!   rather than a rebuild;
//! - [`ffi`] is the RAII layer over the C API, and [`backend`] is the
//!   `RenderBackend` implementation on top of it.
//!
//! [`cache`] sits beside them: decoded preview frames, kept so that stepping
//! backwards does not re-seek and re-decode a GOP per frame.
//!
//! `libmlt` is linked dynamically and `melt`/`melted` are never vendored:
//! davimci is GPL-3.0 over LGPL-2.1 MLT.

pub mod backend;
mod cache;
mod convert;
pub mod error;
pub mod ffi;
pub mod patch;
pub mod projection;
pub mod transitions;
pub mod xml;

pub use backend::MltBackend;
pub use error::MltError;
pub use projection::Projection;
pub use xml::to_xml;
