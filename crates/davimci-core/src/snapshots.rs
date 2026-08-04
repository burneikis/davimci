//! Snapshot tests on the compact timeline dump.
//!
//! These exist so ripple/lift/trim diffs are readable in review: a behaviour
//! change shows up as a one-line diff of the timeline itself.

#![cfg(test)]
#![allow(clippy::unwrap_used)]

use insta::assert_snapshot;

use crate::testing::{clip_ids, media_fixture, track_id};
use crate::time::Frame;
use crate::timeline::Timeline;
use crate::trim::Edge;

fn scene() -> Timeline {
    media_fixture(&[(0, 100, 50, 400), (100, 80, 50, 400), (180, 120, 50, 400)])
}

#[test]
fn initial_scene() {
    assert_snapshot!(scene().dump(), @r"
    V1:[m0 0-100][m1 100-180][m2 180-300]
    A1: -
    ");
}

#[test]
fn split_then_ripple_delete() {
    let mut tl = scene();
    let v1 = track_id(&tl, "V1");
    tl.split_at(v1, Frame(40)).unwrap();
    tl.ripple_delete_range(v1, Frame(40), Frame(140)).unwrap();
    assert_snapshot!(tl.dump(), @r"
    V1:[m0 0-40][m1 40-80][m2 80-200]
    A1: -
    ");
}

#[test]
fn lift_leaves_a_hole() {
    let mut tl = scene();
    let v1 = track_id(&tl, "V1");
    tl.lift_range(v1, Frame(40), Frame(140)).unwrap();
    assert_snapshot!(tl.dump(), @r"
    V1:[m0 0-40]<gap 100>[m1 140-180][m2 180-300]
    A1: -
    ");
}

#[test]
fn trim_roll_slide_sequence() {
    let mut tl = scene();
    let v1 = track_id(&tl, "V1");
    let ids = clip_ids(&tl, "V1");
    tl.ripple_trim(v1, ids[0], Edge::Tail, -20).unwrap();
    tl.roll(v1, Frame(80), 10).unwrap();
    tl.slide(v1, ids[1], -5).unwrap();
    assert_snapshot!(tl.dump(), @r"
    V1:[m0 0-85][m1 85-155][m2 155-280]
    A1: -
    ");
}

#[test]
fn linked_clips_are_marked_in_the_dump() {
    let mut tl = crate::testing::fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "a-aud")])]);
    let v = clip_ids(&tl, "V1")[0];
    let a = clip_ids(&tl, "A1")[0];
    tl.link(&[v, a]).unwrap();
    assert_snapshot!(tl.dump(), @r"
    V1:[a 0-100 g5]
    A1:[a-aud 0-100 g5]
    ");
}
