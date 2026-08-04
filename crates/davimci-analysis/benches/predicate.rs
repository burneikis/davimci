//! Spec 14's "predicate motions never scan", measured (plan.md 2).
//!
//! An hour of audio at a 10 ms hop is 360 000 hops. If a lookup were a scan,
//! the two lengths benchmarked here would differ by an order of magnitude;
//! they must not.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use davimci_analysis::{Analysis, AnalysisIndex, AnalysisParams, Hop, Span};
use davimci_core::{Fps, Frame, TrackId};
use davimci_motion::{Direction, Predicate, PredicateIndex};

fn analysis(minutes: u64) -> Analysis {
    let params = AnalysisParams::default();
    let duration_ms = minutes * 60_000;
    let hops = (0..duration_ms / u64::from(params.hop_ms))
        .map(|i| {
            // A quiet floor with a loud hop every second, so a threshold
            // query has matches to find rather than a single trivial one.
            let loud = i % 100 == 0;
            Hop {
                peak_db: if loud { -1.0 } else { -60.0 },
                rms_db: if loud { -6.0 } else { -70.0 },
            }
        })
        .collect();
    let silence = (0..minutes * 6)
        .map(|i| Span {
            start_ms: i * 10_000,
            end_ms: i * 10_000 + 500,
        })
        .collect();
    Analysis {
        version: 1,
        source_hash: "bench".to_string(),
        params,
        sample_rate: 48_000,
        duration_ms,
        hops,
        silence,
        scene_changes: (0..minutes * 60).map(|i| i * 1_000).collect(),
    }
}

fn index(minutes: u64) -> AnalysisIndex {
    let mut idx = AnalysisIndex::new(Fps::FPS_60);
    idx.insert(TrackId(1), &analysis(minutes));
    idx
}

fn predicate_lookup(c: &mut Criterion) {
    for minutes in [1u64, 60] {
        let idx = index(minutes);
        let from = Frame(minutes * 60 * 60 / 2);
        c.bench_function(&format!("predicate/peak {minutes}min"), |b| {
            b.iter(|| {
                idx.find(
                    &Predicate::AudioPeak {
                        track: TrackId(1),
                        threshold_db: -2.0,
                    },
                    from,
                    Direction::Forward,
                )
            });
        });
        c.bench_function(&format!("predicate/silence {minutes}min"), |b| {
            b.iter(|| {
                idx.find(
                    &Predicate::Silence {
                        track: TrackId(1),
                        min_duration_ms: 300,
                        threshold_db: -50.0,
                    },
                    from,
                    Direction::Forward,
                )
            });
        });
    }
}

criterion_group!(benches, predicate_lookup);
criterion_main!(benches);
