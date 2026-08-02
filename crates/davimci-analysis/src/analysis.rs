//! The analysis pass (spec §10.2, plan.md Phase 5).
//!
//! Everything is precomputed on import: peak and RMS at a fixed hop, silence
//! spans, and optional scene-change points. Nothing is analysed while
//! scrubbing, so predicate motions are an indexed lookup rather than a scan.
//!
//! The measurement functions are pure over sample slices, which is what makes
//! the ground truth in `scripts/gen-fixtures.sh` assertable: a fixture with
//! tone from 1-2s must produce silence spans that agree to within one hop.

use serde::{Deserialize, Serialize};

/// Bump to invalidate every cached analysis on disk.
pub const ANALYSIS_VERSION: u32 = 1;

/// Analysis settings. The 10 ms hop is the spec §10.2 default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AnalysisParams {
    pub hop_ms: u32,
    /// Below this RMS, a hop counts as silent.
    pub silence_threshold_db: f32,
    /// Silence shorter than this is not a span, just a gap between words.
    pub min_silence_ms: u32,
}

impl Default for AnalysisParams {
    fn default() -> Self {
        Self {
            hop_ms: 10,
            silence_threshold_db: -50.0,
            min_silence_ms: 100,
        }
    }
}

/// One hop's measurements. Both are dB relative to full scale.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Hop {
    pub peak_db: f32,
    pub rms_db: f32,
}

/// A half-open span of source time, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl Span {
    #[must_use]
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Everything known about one source's media.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub version: u32,
    /// Content hash of the source, so a cache entry cannot be mismatched.
    pub source_hash: String,
    pub params: AnalysisParams,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub hops: Vec<Hop>,
    pub silence: Vec<Span>,
    /// Scene-change points in milliseconds (optional; empty for audio).
    pub scene_changes: Vec<u64>,
}

impl Analysis {
    /// Start of hop `i`, in milliseconds.
    #[must_use]
    pub fn hop_start_ms(&self, i: usize) -> u64 {
        i as u64 * u64::from(self.params.hop_ms)
    }

    /// An analysis with no audio content, used for video-only sources.
    #[must_use]
    pub fn empty(source_hash: impl Into<String>, params: AnalysisParams) -> Self {
        Self {
            version: ANALYSIS_VERSION,
            source_hash: source_hash.into(),
            params,
            sample_rate: 0,
            duration_ms: 0,
            hops: Vec::new(),
            silence: Vec::new(),
            scene_changes: Vec::new(),
        }
    }
}

/// The floor for a fully silent hop. Below this, dB is meaningless.
pub const SILENT_DB: f32 = -120.0;

#[must_use]
fn to_db(linear: f32) -> f32 {
    if linear <= 0.0 {
        SILENT_DB
    } else {
        (20.0 * linear.log10()).max(SILENT_DB)
    }
}

/// Analyse mono samples in `[-1, 1]`.
///
/// Callers downmix first: analysis answers "is there sound here", which is a
/// property of the take, not of the channel layout.
#[must_use]
pub fn analyze_samples(samples: &[f32], sample_rate: u32, params: AnalysisParams) -> Analysis {
    let hop = (u64::from(sample_rate) * u64::from(params.hop_ms) / 1000).max(1) as usize;
    let mut hops = Vec::with_capacity(samples.len() / hop + 1);
    for chunk in samples.chunks(hop) {
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        for s in chunk {
            peak = peak.max(s.abs());
            sum_sq += f64::from(*s) * f64::from(*s);
        }
        let rms = (sum_sq / chunk.len().max(1) as f64).sqrt() as f32;
        hops.push(Hop {
            peak_db: to_db(peak),
            rms_db: to_db(rms),
        });
    }
    let duration_ms = if sample_rate == 0 {
        0
    } else {
        samples.len() as u64 * 1000 / u64::from(sample_rate)
    };
    let silence = silence_spans(&hops, params);
    Analysis {
        version: ANALYSIS_VERSION,
        source_hash: String::new(),
        params,
        sample_rate,
        duration_ms,
        hops,
        silence,
        scene_changes: Vec::new(),
    }
}

/// Merge runs of quiet hops into spans, dropping the ones too short to be
/// worth jumping to.
#[must_use]
pub fn silence_spans(hops: &[Hop], params: AnalysisParams) -> Vec<Span> {
    let hop_ms = u64::from(params.hop_ms.max(1));
    let mut spans = Vec::new();
    let mut run: Option<u64> = None;
    for (i, h) in hops.iter().enumerate() {
        let quiet = h.rms_db < params.silence_threshold_db;
        match (quiet, run) {
            (true, None) => run = Some(i as u64),
            (false, Some(start)) => {
                spans.push(Span {
                    start_ms: start * hop_ms,
                    end_ms: i as u64 * hop_ms,
                });
                run = None;
            }
            _ => {}
        }
    }
    if let Some(start) = run {
        spans.push(Span {
            start_ms: start * hop_ms,
            end_ms: hops.len() as u64 * hop_ms,
        });
    }
    spans.retain(|s| s.duration_ms() >= u64::from(params.min_silence_ms));
    spans
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
pub(crate) mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// The `tone_gaps.wav` fixture, synthesised: a 1 kHz tone at half scale
    /// during 1-2s and 3-4s, silence elsewhere, 5s at 48 kHz.
    pub(crate) fn tone_gaps(sample_rate: u32) -> Vec<f32> {
        (0..sample_rate * 5)
            .map(|n| {
                let t = n as f32 / sample_rate as f32;
                if (1.0..2.0).contains(&t) || (3.0..4.0).contains(&t) {
                    0.5 * (1000.0 * 2.0 * PI * t).sin()
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn analysis() -> Analysis {
        analyze_samples(&tone_gaps(48_000), 48_000, AnalysisParams::default())
    }

    #[test]
    fn hops_cover_the_whole_source_at_the_configured_rate() {
        let a = analysis();
        assert_eq!(a.params.hop_ms, 10);
        assert_eq!(a.hops.len(), 500, "5s at a 10ms hop");
        assert_eq!(a.duration_ms, 5000);
        assert_eq!(a.hop_start_ms(300), 3000);
    }

    /// plan.md Phase 5: silence spans within one hop of ground truth.
    #[test]
    fn silence_spans_match_the_fixture_to_within_one_hop() {
        let a = analysis();
        let want = [(0, 1000), (2000, 3000), (4000, 5000)];
        assert_eq!(a.silence.len(), want.len(), "got {:?}", a.silence);
        for (span, (start, end)) in a.silence.iter().zip(want) {
            assert!(
                span.start_ms.abs_diff(start) <= 10 && span.end_ms.abs_diff(end) <= 10,
                "{span:?} is more than one hop from {start}-{end}"
            );
        }
    }

    /// plan.md Phase 5: peak detection finds the exact tone frames.
    #[test]
    fn the_tone_hops_peak_at_exactly_half_scale() {
        let a = analysis();
        // -6.02 dB is 0.5 linear.
        let loud: Vec<usize> = a
            .hops
            .iter()
            .enumerate()
            .filter(|(_, h)| h.peak_db > -12.0)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(loud.first(), Some(&100), "tone starts at 1.000s");
        assert_eq!(loud.last(), Some(&399), "tone ends at 4.000s");
        assert!((a.hops[150].peak_db - -6.02).abs() < 0.05);
        assert!(
            (a.hops[150].rms_db - -9.03).abs() < 0.1,
            "a sine's RMS is 3 dB below its peak"
        );
    }

    #[test]
    fn pure_silence_is_one_span_at_the_floor() {
        let a = analyze_samples(&vec![0.0; 48_000], 48_000, AnalysisParams::default());
        assert_eq!(a.silence.len(), 1);
        assert_eq!(a.silence[0].duration_ms(), 1000);
        assert_eq!(a.hops[0].rms_db, SILENT_DB);
    }

    #[test]
    fn a_gap_shorter_than_the_minimum_is_not_a_span() {
        let params = AnalysisParams {
            min_silence_ms: 500,
            ..AnalysisParams::default()
        };
        let mut samples = vec![0.5f32; 48_000];
        // 200ms of silence in the middle: real, but not worth jumping to.
        samples[20_000..29_600].fill(0.0);
        let a = analyze_samples(&samples, 48_000, params);
        assert!(a.silence.is_empty(), "got {:?}", a.silence);
    }

    #[test]
    fn analysis_of_nothing_is_empty_not_a_panic() {
        let a = analyze_samples(&[], 48_000, AnalysisParams::default());
        assert!(a.hops.is_empty());
        assert!(a.silence.is_empty());
        assert_eq!(a.duration_ms, 0);
        // A zero sample rate must not divide by zero either.
        let _ = analyze_samples(&[0.1, 0.2], 0, AnalysisParams::default());
    }
}
