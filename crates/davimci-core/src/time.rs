//! Exact frame-based time (spec §7.1, plan.md Phase 1).
//!
//! A project has exactly one framerate. All positions are whole frame counts,
//! so there is one and only one notion of "frame N" and edit points cannot
//! drift between tracks. Floats appear only at display and FFI boundaries.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::CoreError;

/// A position or duration on the timeline, in whole timeline frames.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub struct Frame(pub u64);

impl Frame {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Saturating add; the timeline has no negative time.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An exact rational framerate. 23.976 is 24000/1001, never 23.976f64.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Fps {
    pub num: u32,
    pub den: u32,
}

impl Fps {
    pub const FPS_24: Self = Self { num: 24, den: 1 };
    pub const FPS_25: Self = Self { num: 25, den: 1 };
    pub const FPS_30: Self = Self { num: 30, den: 1 };
    pub const FPS_50: Self = Self { num: 50, den: 1 };
    pub const FPS_60: Self = Self { num: 60, den: 1 };
    pub const FPS_23_976: Self = Self {
        num: 24_000,
        den: 1001,
    };
    pub const FPS_29_97: Self = Self {
        num: 30_000,
        den: 1001,
    };
    pub const FPS_59_94: Self = Self {
        num: 60_000,
        den: 1001,
    };

    /// Construct a framerate, rejecting a zero numerator or denominator.
    pub fn new(num: u32, den: u32) -> Result<Self, CoreError> {
        if num == 0 || den == 0 {
            return Err(CoreError::NoFramerate);
        }
        Ok(Self { num, den })
    }

    #[must_use]
    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Convert a frame count at this rate to nanoseconds, exactly.
    #[must_use]
    pub fn frame_to_nanos(self, frame: Frame) -> u128 {
        // frame * den / num * 1e9, ordered to avoid precision loss.
        frame.0 as u128 * u128::from(self.den) * 1_000_000_000 / u128::from(self.num)
    }

    /// Nearest-frame mapping of a source frame at `from` onto this rate.
    ///
    /// This is the default conform rule from spec §7.1: exact whole frames,
    /// no accumulation of rounding error, because each frame is mapped
    /// independently from its own timestamp rather than by stepping.
    #[must_use]
    pub fn conform_frame(self, source_frame: Frame, from: Fps) -> Frame {
        if from == self {
            return source_frame;
        }
        // round(source_frame * (self / from))
        let num = source_frame.0 as u128 * u128::from(self.num) * u128::from(from.den);
        let den = u128::from(self.den) * u128::from(from.num);
        Frame(((num + den / 2) / den) as u64)
    }
}

impl fmt::Display for Fps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{:.3}", self.as_f64())
        }
    }
}

/// Frame dimensions of the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub const HD_1080: Self = Self {
        width: 1920,
        height: 1080,
    };

    #[must_use]
    pub fn aspect(self) -> f64 {
        f64::from(self.width) / f64::from(self.height)
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.width, self.height)
    }
}

/// Project-level properties every clip is conformed to (spec §7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineProps {
    pub fps: Fps,
    pub resolution: Resolution,
    pub sample_rate: u32,
}

impl Default for TimelineProps {
    /// The spec §14 baseline: 1080p60.
    fn default() -> Self {
        Self {
            fps: Fps::FPS_60,
            resolution: Resolution::HD_1080,
            sample_rate: 48_000,
        }
    }
}

impl fmt::Display for TimelineProps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} @ {} fps, {} Hz",
            self.resolution, self.fps, self.sample_rate
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn ntsc_rates_are_exact_rationals() {
        assert_eq!(Fps::FPS_23_976.num, 24_000);
        assert_eq!(Fps::FPS_23_976.den, 1001);
        assert!((Fps::FPS_23_976.as_f64() - 23.976).abs() < 0.001);
    }

    #[test]
    fn zero_framerate_is_rejected() {
        assert!(Fps::new(0, 1).is_err());
        assert!(Fps::new(30, 0).is_err());
        assert!(Fps::new(30, 1).is_ok());
    }

    #[test]
    fn identity_conform_is_lossless() {
        let f = Frame(123_456);
        assert_eq!(Fps::FPS_60.conform_frame(f, Fps::FPS_60), f);
    }

    #[test]
    fn integer_ratio_conform_is_exact() {
        // 30 -> 60 doubles.
        assert_eq!(
            Fps::FPS_60.conform_frame(Frame(100), Fps::FPS_30),
            Frame(200)
        );
        // 60 -> 30 halves.
        assert_eq!(
            Fps::FPS_30.conform_frame(Frame(200), Fps::FPS_60),
            Frame(100)
        );
    }

    #[test]
    fn ntsc_conform_stays_within_half_a_frame() {
        // Nearest-frame mapping is computed independently per frame, so error
        // is bounded at half a frame no matter how far into the timeline we
        // are - it never accumulates. Compare against the true instant rather
        // than a hand-rounded frame number.
        for src in [Frame(1), Frame(1_000), Frame(86_313), Frame(863_130)] {
            let out = Fps::FPS_60.conform_frame(src, Fps::FPS_23_976);
            let exact = src.0 as f64 * Fps::FPS_23_976.den as f64 / Fps::FPS_23_976.num as f64
                * Fps::FPS_60.as_f64();
            let drift = (out.0 as f64 - exact).abs();
            assert!(
                drift <= 0.5,
                "frame {src}: drifted {drift} frames (exact {exact})"
            );
        }
    }

    #[test]
    fn conform_is_monotonic() {
        // Adjacent source frames must never map backwards.
        let mut prev = Frame::ZERO;
        for f in 0..10_000u64 {
            let out = Fps::FPS_30.conform_frame(Frame(f), Fps::FPS_59_94);
            assert!(out >= prev, "frame {f} mapped backwards: {out} < {prev}");
            prev = out;
        }
    }

    #[test]
    fn display_is_human_readable() {
        assert_eq!(Fps::FPS_60.to_string(), "60");
        assert_eq!(Fps::FPS_23_976.to_string(), "23.976");
        assert_eq!(Resolution::HD_1080.to_string(), "1920x1080");
        assert_eq!(
            TimelineProps::default().to_string(),
            "1920x1080 @ 60 fps, 48000 Hz"
        );
    }

    #[test]
    fn frame_arithmetic_never_goes_negative() {
        assert_eq!(Frame(5).saturating_sub(Frame(10)), Frame::ZERO);
    }

    proptest! {
        /// Conform must always land on a whole frame and never panic,
        /// for any rate pair. (Whole-frame is structural: Frame is u64.)
        #[test]
        fn conform_is_total(
            f in 0u64..10_000_000,
            a_num in 1u32..=120_000, a_den in 1u32..=1001,
            b_num in 1u32..=120_000, b_den in 1u32..=1001,
        ) {
            let from = Fps { num: a_num, den: a_den };
            let to = Fps { num: b_num, den: b_den };
            let _ = to.conform_frame(Frame(f), from);
        }

        /// Round-tripping through a faster rate and back is within one frame.
        #[test]
        fn conform_round_trip_is_stable(f in 0u64..1_000_000) {
            let there = Fps::FPS_60.conform_frame(Frame(f), Fps::FPS_30);
            let back = Fps::FPS_30.conform_frame(there, Fps::FPS_60);
            prop_assert_eq!(back, Frame(f));
        }
    }
}
