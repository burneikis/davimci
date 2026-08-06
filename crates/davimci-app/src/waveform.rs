//! Waveform data for audio lanes.
//!
//! The measurements come from `davimci-analysis`, which measures the
//! *source*, so everything here is indexed by source time. Mapping a screen
//! column back to source time is the view's job, not a frontend's: a GUI and
//! a TUI must draw the same envelope at different resolutions, and only one
//! of them may own the arithmetic.

use std::collections::BTreeMap;

use davimci_core::TrackId;

/// The number of levels a column is quantised to. Integral on purpose: the
/// cross-frontend parity test compares rendered output byte for byte.
pub const LEVELS: u8 = u8::MAX;

/// One source's peak envelope, as published by the host.
#[derive(Debug, Clone, PartialEq)]
pub struct Waveform {
    /// Milliseconds covered by each peak.
    pub hop_ms: u32,
    /// Peak amplitude per hop, `0.0..=1.0`.
    pub peaks: Vec<f32>,
}

impl Waveform {
    /// Build from the decibel peaks `davimci-analysis` produces.
    ///
    /// Silence is `-inf` dB in that representation, so the conversion has to
    /// floor rather than divide: a `-90 dB` hop is drawn as nothing, not as a
    /// negative bar.
    #[must_use]
    pub fn from_db(hop_ms: u32, peaks_db: &[f32]) -> Self {
        Self {
            hop_ms: hop_ms.max(1),
            peaks: peaks_db
                .iter()
                .map(|db| {
                    if !db.is_finite() || *db <= -90.0 {
                        0.0
                    } else {
                        (10f32.powf(db / 20.0)).clamp(0.0, 1.0)
                    }
                })
                .collect(),
        }
    }

    /// The loudest peak in `[from_ms, to_ms)`, quantised to `0..=LEVELS`.
    ///
    /// A column narrower than a hop still reads that hop: dropping it would
    /// make a zoomed-in waveform flicker between silence and signal.
    #[must_use]
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to 0..=LEVELS and rounded before the conversion"
    )]
    pub fn level(&self, from_ms: u64, to_ms: u64) -> u8 {
        if self.peaks.is_empty() {
            return 0;
        }
        let hop = u64::from(self.hop_ms);
        let index = |ms: u64| usize::try_from(ms / hop).unwrap_or(usize::MAX);
        let first = index(from_ms);
        let last = index(to_ms.max(from_ms + 1).saturating_sub(1));
        if first >= self.peaks.len() {
            return 0;
        }
        let last = last.min(self.peaks.len() - 1);
        let peak = self.peaks[first..=last]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        // Clamped to 0..=LEVELS first, so the rounded value is always a
        // level and the conversion cannot lose anything.
        let level = (peak.clamp(0.0, 1.0) * f32::from(LEVELS)).round();
        level as u8
    }
}

/// Every track's published waveform. Absent means "not analysed yet", which
/// is drawn as an empty lane rather than as silence.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Waveforms {
    tracks: BTreeMap<TrackId, Waveform>,
}

impl Waveforms {
    pub fn insert(&mut self, track: TrackId, waveform: Waveform) {
        self.tracks.insert(track, waveform);
    }

    /// Drop a track's envelope: its audio changed, so the old one is a lie
    /// until analysis re-runs.
    pub fn invalidate(&mut self, track: TrackId) {
        self.tracks.remove(&track);
    }

    #[must_use]
    pub fn get(&self, track: TrackId) -> Option<&Waveform> {
        self.tracks.get(&track)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(
    clippy::float_cmp,
    reason = "the values under test are set exactly, so exact equality is the assertion"
)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_floored_rather_than_drawn_as_a_negative_bar() {
        let w = Waveform::from_db(10, &[f32::NEG_INFINITY, -90.0, -6.0, 0.0]);
        assert_eq!(w.peaks[0], 0.0);
        assert_eq!(w.peaks[1], 0.0);
        assert!((w.peaks[2] - 0.501).abs() < 0.01);
        assert_eq!(w.peaks[3], 1.0);
    }

    #[test]
    fn a_column_reads_the_loudest_hop_it_covers() {
        let w = Waveform::from_db(10, &[-60.0, 0.0, -60.0]);
        assert_eq!(w.level(0, 30), LEVELS, "the loud hop wins the column");
        assert_eq!(w.level(0, 10), 0);
        assert_eq!(w.level(10, 20), LEVELS);
    }

    #[test]
    fn a_column_narrower_than_a_hop_still_reads_it() {
        // Otherwise a zoomed-in waveform flickers between signal and nothing.
        let w = Waveform::from_db(100, &[0.0]);
        assert_eq!(w.level(0, 1), LEVELS);
        assert_eq!(w.level(50, 51), LEVELS);
    }

    #[test]
    fn past_the_end_reads_as_nothing_rather_than_panicking() {
        let w = Waveform::from_db(10, &[0.0]);
        assert_eq!(w.level(1_000, 1_010), 0);
    }
}
