//! 1080p60 stays inside the frame budget.
//!
//! This measures the presenter's own cost - pull, compose, letterbox - over a
//! synthetic source, so a regression here is davimci's and not the decoder's.
//! Decode cost is measured by the slow export tests instead. Ignored by
//! default and run in release by `just perf`.

// A debug build times nothing meaningful, so these do not exist in one.
#![cfg(not(debug_assertions))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use davimci_backend::{MockBackend, PreviewScale, RenderBackend};

use davimci_core::{Fps, Frame, Resolution};
use davimci_present::HeadlessPresenter;

#[test]
#[ignore = "timing budget; run in release via `just perf`"]
fn composing_1080p60_costs_a_fraction_of_a_frame() {
    let full = Resolution {
        width: 1920,
        height: 1080,
    };
    let mut backend = MockBackend::new(full);
    backend
        .preview_start(Frame::ZERO, PreviewScale::Full)
        .expect("preview starts");
    let mut presenter = HeadlessPresenter::new(full, Fps::FPS_60);

    const TICKS: u32 = 600; // ten seconds of playback

    // What the source alone costs. The mock synthesises a 1080p buffer per
    // frame, which a real decoder would too, and that cost is not the
    // presenter's - so it is measured and subtracted rather than budgeted.
    let start = Instant::now();
    for i in 0..TICKS {
        backend
            .frame_at(Frame(u64::from(i)), PreviewScale::Full)
            .expect("the mock always has a frame");
    }
    let source = start.elapsed() / TICKS;

    backend.seek(Frame::ZERO).expect("seek");
    let start = Instant::now();
    for _ in 0..TICKS {
        presenter.tick(&mut backend).expect("tick");
    }
    let per_frame = start.elapsed() / TICKS;
    let presenting = per_frame.saturating_sub(source);

    // A quarter of the 16.6 ms budget: the presenter must leave room for the
    // decoder, the GUI, and everything else that shares the frame.
    let budget = Duration::from_micros(4_100);
    assert!(
        presenting <= budget,
        "1080p60 presentation took {presenting:?} per frame on top of {source:?} of source, \
         budget {budget:?}"
    );
    let stats = presenter.presenter().stats();
    assert_eq!(stats.presented + stats.repeated, u64::from(TICKS));
}
