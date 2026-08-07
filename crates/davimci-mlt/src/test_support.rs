//! Serialisation of real-media tests across a test binary's threads.
//!
//! MLT's avformat producer shares ffmpeg demuxer state that is only guarded
//! per-context: opening and decoding two files concurrently in one process
//! trips ffmpeg's `fctx->async_lock` assertion and aborts the whole binary.
//! davimci never decodes from two threads in the editor, so the constraint
//! only shows up under the test harness, where every `#[test]` runs in
//! parallel. Any test that builds a graph over real media holds this lock.

use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

/// Hold this for as long as the test touches MLT.
///
/// Poison is ignored on purpose: a panicking test is a failing test, not a
/// reason to abort the rest of the binary, and there is no shared Rust state
/// behind the lock to be left inconsistent.
pub fn media_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}
