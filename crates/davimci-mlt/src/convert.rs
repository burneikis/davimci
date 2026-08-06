//! Conversions across the MLT ABI boundary.
//!
//! MLT counts frames, sizes and stream indices in C `int`. Nothing in the
//! model does, so every crossing is a narrowing or a widening; doing it with
//! `as` would turn a position MLT cannot address into a negative one and feed
//! it back as a valid frame. These saturate instead, so an out-of-range value
//! is clamped at the edge of what MLT can express rather than wrapped.

use std::ffi::c_int;

/// A model-side count or index as MLT's `int`.
pub(crate) fn mlt_int<T: TryInto<c_int>>(value: T) -> c_int {
    value.try_into().unwrap_or(c_int::MAX)
}

/// A frame position MLT reported. Negative means "no position" in MLT, which
/// is frame zero here.
pub(crate) fn frames(value: c_int) -> u64 {
    u64::from(size(value))
}

/// A size or index MLT reported, which is never meaningfully negative.
pub(crate) fn size(value: c_int) -> u32 {
    value.max(0).unsigned_abs()
}

/// A count MLT reported, for indexing into a Rust collection.
pub(crate) fn count(value: c_int) -> usize {
    size(value) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_position_mlt_cannot_address_saturates_instead_of_wrapping() {
        assert_eq!(mlt_int(u64::MAX), c_int::MAX);
        assert_eq!(mlt_int(5u64), 5);
    }

    #[test]
    fn a_negative_position_reads_back_as_frame_zero() {
        assert_eq!(frames(-1), 0);
        assert_eq!(frames(7), 7);
        assert_eq!(size(-3), 0);
        assert_eq!(count(-3), 0);
    }
}
