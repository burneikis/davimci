//! How many threads MLT is asked to decode and to play with.
//!
//! Both numbers are policy, not mechanism, so they are pure functions of the
//! core count and can be asserted without MLT, media or a device.
//!
//! They matter because MLT's defaults are for a machine embedding a player,
//! not for an editor: the `avformat` producer decodes with one thread unless
//! told otherwise, and a consumer's `real_time` is a thread count as well as
//! a frame-dropping switch, so the value 1 buys one processing thread for the
//! whole graph - decode, rescale, colour convert and blend all on one core.
//!
//! `DAVIMCI_DECODE_THREADS` and `DAVIMCI_REAL_TIME` override the choice.
//! They exist so `scripts/bench-preview.sh` can measure this policy against
//! MLT's defaults on the machine complaining about it; a value that does not
//! parse is ignored rather than fatal.

/// MLT's own ceiling on the `threads` property of the `avformat` producer.
const MAX_DECODE_THREADS: usize = 4;

/// Cores this machine reports, or one when it will not say.
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
}

/// The `threads` property for an `avformat` producer.
///
/// MLT documents the maximum as 4 and refuses to read more, so a 32-core box
/// asks for 4 rather than for a number the producer would clamp silently.
#[must_use]
pub fn decode_threads_for(cores: usize) -> i32 {
    let n = cores.clamp(1, MAX_DECODE_THREADS);
    i32::try_from(n).unwrap_or(1)
}

/// The `real_time` property for the preview consumer.
///
/// Positive means "this many processing threads, dropping frames that cannot
/// be made in time", and dropping is what a preview owned by a wall clock
/// wants. The count stays at one because raising it was measured and lost:
/// on a 2160p60 single-clip timeline, four processing threads showed 33.6
/// frames/s against 34.5 for one, since the graph is a decode and a copy
/// with nothing to run beside them. A filter-heavy timeline is the case that
/// could pay for the threads, so `DAVIMCI_REAL_TIME` can raise it and
/// `scripts/bench-preview.sh` can settle it, but the default follows the
/// numbers this machine gave.
#[must_use]
pub fn preview_real_time_for(cores: usize) -> i32 {
    let _ = cores;
    1
}

/// An override from the environment, when it is there and is a number.
fn override_of(key: &str) -> Option<i32> {
    std::env::var(key).ok()?.trim().parse().ok()
}

/// [`decode_threads_for`] on this machine, unless overridden.
#[must_use]
pub fn decode_threads() -> i32 {
    override_of("DAVIMCI_DECODE_THREADS").unwrap_or_else(|| decode_threads_for(cores()))
}

/// [`preview_real_time_for`] on this machine, unless overridden.
#[must_use]
pub fn preview_real_time() -> i32 {
    override_of("DAVIMCI_REAL_TIME").unwrap_or_else(|| preview_real_time_for(cores()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_core_machine_asks_for_one_thread_of_each() {
        assert_eq!(decode_threads_for(1), 1);
        assert_eq!(preview_real_time_for(1), 1);
    }

    #[test]
    fn decode_threads_never_exceed_what_mlt_accepts() {
        assert_eq!(decode_threads_for(64), 4);
    }

    /// Regression: parallel frame processing was raised with the decode
    /// threads and measured slower on a single-clip 2160p timeline. It stays
    /// at one until a measurement says otherwise.
    #[test]
    fn preview_processing_stays_single_threaded_and_dropping() {
        for c in 1..=64 {
            assert_eq!(preview_real_time_for(c), 1, "{c} cores changed real_time");
        }
    }

    #[test]
    fn every_multicore_machine_decodes_on_more_than_mlts_one_thread() {
        for c in 2..=16 {
            assert!(decode_threads_for(c) > 1, "{c} cores decoded on one thread");
        }
    }
}
