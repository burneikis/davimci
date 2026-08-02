//! Presenter errors (Phase 0: complete user-facing sentences).

#[derive(Debug, thiserror::Error)]
pub enum PresentError {
    #[error("The preview could not pull a frame: {0}")]
    Pull(String),
    #[error("A video frame arrived malformed ({width}x{height}, {bytes} bytes) and was not shown.")]
    MalformedFrame {
        width: u32,
        height: u32,
        bytes: usize,
    },
}
