//! Wall-clock ceilings: edits stay instant on a few hundred clips.
//!
//! Criterion measures; this asserts. The budgets sit roughly an order of
//! magnitude above the measured times in `benches/edits.rs`, so they catch an
//! accidental quadratic rather than a slow machine. Ignored by default and
//! run in release by `just perf`, because a debug build proves nothing.

// A debug build times nothing meaningful, so these do not exist in one.
#![cfg(not(debug_assertions))]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use davimci_cmd::{EditCommand, ProjectFile, Session};
use davimci_core::{Frame, Timeline, TrackId, testing::fixture};

fn big_timeline(n: u64) -> Timeline {
    let labels: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
    let clips: Vec<(u64, u64, &str)> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| (i as u64 * 100, 100, l.as_str()))
        .collect();
    fixture(&[("V1", &clips)])
}

/// Interior frames to split at: a clip boundary has nothing to split.
fn split_frames(n: usize) -> Vec<Frame> {
    (1u64..)
        .map(|i| i * 37 + 1)
        .filter(|f| f % 100 != 0)
        .take(n)
        .map(Frame)
        .collect()
}

fn v1(tl: &Timeline) -> TrackId {
    tl.track_by_name("V1").map(|t| t.id).unwrap_or(TrackId(0))
}

#[track_caller]
fn within(budget: Duration, what: &str, f: impl FnOnce()) {
    let start = Instant::now();
    f();
    let took = start.elapsed();
    assert!(took <= budget, "{what} took {took:?}, budget {budget:?}");
}

#[test]
#[ignore = "timing budget; run in release via `just perf`"]
fn a_ripple_delete_on_five_hundred_clips_is_instant() {
    let tl = big_timeline(500);
    let track = v1(&tl);
    let mut session = Session::new(tl);
    within(Duration::from_millis(5), "ripple delete", || {
        session
            .exec(&EditCommand::RippleDelete {
                track,
                start: Frame(10_000),
                end: Frame(10_100),
            })
            .unwrap();
    });
}

#[test]
#[ignore = "timing budget; run in release via `just perf`"]
fn undoing_five_hundred_edits_is_instant() {
    let tl = big_timeline(200);
    let track = v1(&tl);
    let mut session = Session::new(tl);
    for frame in split_frames(500) {
        session
            .exec(&EditCommand::Split {
                track,
                frame,
                new_id: None,
            })
            .unwrap();
    }
    within(Duration::from_millis(50), "500 undos", || {
        for _ in 0..500 {
            session.undo().unwrap();
        }
    });
}

#[test]
#[ignore = "timing budget; run in release via `just perf`"]
fn loading_a_large_project_is_under_a_blink() {
    let tl = big_timeline(500);
    let track = v1(&tl);
    let mut session = Session::new(tl);
    for frame in split_frames(200) {
        session
            .exec(&EditCommand::Split {
                track,
                frame,
                new_id: None,
            })
            .unwrap();
    }
    let json = ProjectFile::from_session(&session).to_json().unwrap();
    within(Duration::from_millis(100), "project load", || {
        ProjectFile::from_json(&json)
            .unwrap()
            .into_session()
            .unwrap();
    });
}
