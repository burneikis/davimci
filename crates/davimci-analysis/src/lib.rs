//! Media import, conform, and analysis.
//!
//! The shape of this crate follows one rule: everything that can be a pure
//! function of data is one, and the parts that must touch the outside world
//! ([`probe::FfprobeProber`], [`decode`], [`cache`]) are small and isolated.
//! That is what lets the conform matrix, the stream-to-track mapping, the
//! silence detector and the predicate index all be tested with no media, no
//! ffmpeg, and no timing.
//!
//! The pipeline reads: [`probe`] -> [`conform`] -> [`import`] (one undoable
//! command) -> [`pipeline::queue_analysis`] in the background -> [`index`],
//! which answers predicate motions and says [`davimci_motion::predicate::Answer::Pending`]
//! until it can do so correctly.

pub mod analysis;
pub mod cache;
pub mod conform;
pub mod decode;
pub mod error;
pub mod import;
pub mod index;
pub mod jobs;
pub mod pipeline;
pub mod probe;
pub mod proxy;
pub mod subtitle;

pub use analysis::{ANALYSIS_VERSION, Analysis, AnalysisParams, Hop, Span};
pub use cache::{AnalysisCache, content_hash, entry_key};
pub use conform::{ConformOptions, Conformed, FitPolicy, FitRect};
pub use error::AnalysisError;
pub use import::{
    ImportOptions, ImportPlan, Imported, Placement, StreamMapping, ids_needed, import, plan,
};
pub use index::AnalysisIndex;
pub use jobs::{JobEvent, JobId, JobRunner};
pub use probe::{FfprobeProber, MediaInfo, Prober, StreamInfo, StreamKind};
pub use proxy::{ProxyMap, ProxyPolicy, ProxySpec, export_guard};
pub use subtitle::{Cue, parse_srt, to_srt};
