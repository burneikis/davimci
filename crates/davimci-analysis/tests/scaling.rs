//! Predicate motions never scan.
//!
//! The claim is structural, so the test is a ratio rather than a budget:
//! sixty times the analysis must not cost anything like sixty times the
//! lookup. A scan would blow the bound by an order of magnitude; the log-time
//! index stays near one. Ignored by default and run in release by `just perf`.

// A debug build times nothing meaningful, so these do not exist in one.
#![cfg(not(debug_assertions))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Instant;

use davimci_analysis::{Analysis, AnalysisIndex, AnalysisParams, Hop, Span};
use davimci_core::{Fps, Frame, TrackId};
use davimci_motion::{Answer, Direction, Predicate, PredicateIndex};

fn index(minutes: u64) -> AnalysisIndex {
    let params = AnalysisParams::default();
    let duration_ms = minutes * 60_000;
    let analysis = Analysis {
        version: 1,
        source_hash: "scaling".to_string(),
        params,
        sample_rate: 48_000,
        duration_ms,
        hops: (0..duration_ms / u64::from(params.hop_ms))
            .map(|i| {
                let loud = i % 100 == 0;
                Hop {
                    peak_db: if loud { -1.0 } else { -60.0 },
                    rms_db: if loud { -6.0 } else { -70.0 },
                }
            })
            .collect(),
        silence: (0..minutes * 6)
            .map(|i| Span {
                start_ms: i * 10_000,
                end_ms: i * 10_000 + 500,
            })
            .collect(),
        scene_changes: (0..minutes * 60).map(|i| i * 1_000).collect(),
    };
    let mut idx = AnalysisIndex::new(Fps::FPS_60);
    idx.insert(TrackId(1), &analysis);
    idx
}

fn time_lookups(idx: &AnalysisIndex, from: Frame) -> f64 {
    let predicate = Predicate::AudioPeak {
        track: TrackId(1),
        threshold_db: -2.0,
    };
    // One untimed pass so the comparison is not measuring a cold cache.
    assert!(matches!(
        idx.find(&predicate, from, Direction::Forward),
        Answer::Found(_)
    ));
    let n = 20_000;
    let start = Instant::now();
    for _ in 0..n {
        std::hint::black_box(idx.find(&predicate, from, Direction::Forward));
    }
    start.elapsed().as_secs_f64() / f64::from(n)
}

#[test]
#[ignore = "timing ratio; run in release via `just perf`"]
fn sixty_times_the_analysis_is_not_sixty_times_the_lookup() {
    let small = index(1);
    let large = index(60);
    let a = time_lookups(&small, Frame(1_800));
    let b = time_lookups(&large, Frame(108_000));
    let ratio = b / a;
    assert!(
        ratio < 5.0,
        "lookup scaled by {ratio:.1}x for 60x the analysis; that is a scan"
    );
}
