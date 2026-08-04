//! Editing latency targets, measured.
//!
//! The claim under test is "instant on a few hundred clips": a ripple delete,
//! an undo of a long log, and a project load all have to stay well inside a
//! frame at 60 Hz. The thresholds live in the sibling regression test rather
//! than here, because criterion measures and does not assert.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use davimci_cmd::{EditCommand, ProjectFile, Session};
use davimci_core::{Frame, Timeline, TrackId, testing::fixture};

/// Interior frames to split at: a clip boundary has nothing to split, so a
/// benchmark that used one would measure a rejection rather than an edit.
fn split_frames(n: usize) -> Vec<Frame> {
    (1u64..)
        .map(|i| i * 37 + 1)
        .filter(|f| f % 100 != 0)
        .take(n)
        .map(Frame)
        .collect()
}

/// A single-track timeline of `n` back-to-back 100-frame clips.
fn big_timeline(n: u64) -> Timeline {
    let labels: Vec<String> = (0..n).map(|i| format!("c{i}")).collect();
    let clips: Vec<(u64, u64, &str)> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| (i as u64 * 100, 100, l.as_str()))
        .collect();
    fixture(&[("V1", &clips)])
}

fn v1(tl: &Timeline) -> TrackId {
    tl.track_by_name("V1").map(|t| t.id).unwrap_or(TrackId(0))
}

fn ripple_delete(c: &mut Criterion) {
    let tl = big_timeline(500);
    let v1 = v1(&tl);
    c.bench_function("ripple_delete/500 clips", |b| {
        b.iter_batched(
            || Session::new(tl.clone()),
            |mut s| {
                let _ = s.exec(&EditCommand::RippleDelete {
                    track: v1,
                    start: Frame(10_000),
                    end: Frame(10_100),
                });
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn undo_a_long_log(c: &mut Criterion) {
    let tl = big_timeline(200);
    let v1 = v1(&tl);
    c.bench_function("undo/500 edits", |b| {
        b.iter_batched(
            || {
                let mut s = Session::new(tl.clone());
                for frame in split_frames(500) {
                    s.exec(&EditCommand::Split {
                        track: v1,
                        frame,
                        new_id: None,
                    })
                    .unwrap();
                }
                s
            },
            |mut s| {
                for _ in 0..500 {
                    let _ = s.undo();
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn project_load(c: &mut Criterion) {
    let mut session = Session::new(big_timeline(500));
    let v1 = v1(session.timeline());
    for frame in split_frames(200) {
        session
            .exec(&EditCommand::Split {
                track: v1,
                frame,
                new_id: None,
            })
            .unwrap();
    }
    let json = ProjectFile::from_session(&session).to_json().unwrap();
    c.bench_function("project_load/500 clips, 200 edits", |b| {
        b.iter(|| {
            let file = ProjectFile::from_json(&json).unwrap();
            file.into_session().unwrap()
        });
    });
}

criterion_group!(benches, ripple_delete, undo_a_long_log, project_load);
criterion_main!(benches);
