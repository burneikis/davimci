//! Spec 14's navigation targets, measured (plan.md 2).
//!
//! Jump-point computation runs on every zoom change, and a predicate motion
//! runs on every `]p`; neither may scale with timeline length in a way the
//! user can feel.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use davimci_core::{Frame, Timeline, testing::fixture};
use davimci_motion::{Direction, JumpConfig, JumpPoints, Zoom};

fn big_timeline(n: u64) -> Timeline {
    let labels: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
    let clips: Vec<(u64, u64, &str)> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| (i as u64 * 100, 100, l.as_str()))
        .collect();
    fixture(&[("V1", &clips)])
}

fn build_jump_points(c: &mut Criterion) {
    let tl = big_timeline(500);
    let cfg = JumpConfig::default();
    for level in [0u8, 4, 8] {
        c.bench_function(&format!("jump_points/build zoom {level}"), |b| {
            b.iter(|| JumpPoints::build(&tl, None, Zoom::new(level), &cfg, &[]));
        });
    }
}

/// Stepping is the hot path: it happens per keystroke, over a set that may
/// hold tens of thousands of points at high zoom.
fn step_jump_points(c: &mut Criterion) {
    let tl = big_timeline(500);
    let points = JumpPoints::build(&tl, None, Zoom::new(6), &JumpConfig::default(), &[]);
    c.bench_function("jump_points/step", |b| {
        b.iter(|| points.step(Frame(25_000), Direction::Forward, 1));
    });
}

criterion_group!(benches, build_jump_points, step_jump_points);
criterion_main!(benches);
