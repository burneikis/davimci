//! Pacing against a synthetic clock, end to end through the presenter
//!.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use davimci_backend::{MockBackend, PreviewScale, RenderBackend};
use davimci_core::{Fps, Frame, Resolution};
use davimci_present::{HeadlessPresenter, Host, Pace};

fn res(width: u32, height: u32) -> Resolution {
    Resolution { width, height }
}

fn backend() -> MockBackend {
    let mut b = MockBackend::new(res(4, 2));
    b.preview_start(Frame::ZERO, PreviewScale::Full)
        .expect("preview starts");
    b
}

#[test]
fn a_jittery_source_never_shows_a_frame_out_of_order() {
    let mut b = backend();
    let mut h = HeadlessPresenter::new(res(16, 8), Fps::FPS_60);
    // Alternate starving and delivering: the classic jitter shape.
    for tick in 0..20u64 {
        b.preview_budget = Some(u64::from(tick % 2 == 0));
        h.tick(&mut b).expect("tick");
    }
    let mut last = None;
    for pos in h.positions().into_iter().flatten() {
        if let Some(prev) = last {
            assert!(pos >= prev, "presented {pos:?} after {prev:?}");
        }
        last = Some(pos);
    }
    let stats = h.presenter().stats();
    assert!(stats.repeated > 0, "jitter produced no repeats");
    assert_eq!(stats.presented + stats.repeated, 20);
}

#[test]
fn a_starved_source_holds_the_last_picture_rather_than_going_black() {
    let mut b = backend();
    let mut h = HeadlessPresenter::new(res(16, 8), Fps::FPS_60);
    b.preview_budget = Some(1);
    h.tick(&mut b).expect("tick");
    let first = h.last().expect("a presentation").pixels.clone();
    for _ in 0..5 {
        h.tick(&mut b).expect("tick");
    }
    let held = h.last().expect("a presentation");
    assert_eq!(held.pixels, first);
    assert!(matches!(held.pace, Pace::Repeated(_)));
}

#[test]
fn both_hosts_present_the_same_playback_pixels() {
    let mut ba = backend();
    let mut bb = backend();
    let mut embedded = HeadlessPresenter::with_host(Host::Embedded, res(21, 9), Fps::FPS_60);
    let mut detached = HeadlessPresenter::with_host(Host::Detached, res(21, 9), Fps::FPS_60);
    for _ in 0..8 {
        embedded.tick(&mut ba).expect("tick");
        detached.tick(&mut bb).expect("tick");
    }
    for (a, b) in embedded.frames().iter().zip(detached.frames()) {
        assert_eq!(a.pixels, b.pixels);
        assert_eq!(a.position, b.position);
    }
    assert!(embedded.last().expect("frame").overlay.timecode.is_some());
    assert!(detached.last().expect("frame").overlay.timecode.is_none());
}

#[test]
fn resizing_mid_playback_recomposes_without_dropping_the_picture() {
    let mut b = backend();
    let mut h = HeadlessPresenter::new(res(16, 8), Fps::FPS_60);
    h.tick(&mut b).expect("tick");
    h.presenter_mut().resize(res(40, 40));
    h.tick(&mut b).expect("tick");
    let last = h.last().expect("a presentation");
    assert_eq!(last.surface, res(40, 40));
    assert_eq!(last.quad.width, 40);
    assert_eq!(last.quad.height, 20);
}
