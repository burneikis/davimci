//! Live-graph tests that need MLT but never decode media.
//!
//! Everything here plays generated producers (colour cards and placeholders),
//! so it stays in the fast suite while still exercising the real C API: the
//! graph is built, patched, seeked, and pulled from for real.

#![allow(clippy::unwrap_used, clippy::panic, clippy::expect_used)]

use davimci_backend::{PreviewScale, RenderBackend};
use davimci_core::testing::{clip_ids, fixture, track_id};
use davimci_core::{Frame, Resolution, TimelineProps, TrackKind};
use davimci_mlt::MltBackend;

fn props() -> TimelineProps {
    TimelineProps {
        resolution: Resolution {
            width: 320,
            height: 180,
        },
        ..TimelineProps::default()
    }
}

fn backend() -> MltBackend {
    MltBackend::new(props()).expect("MLT is a build prerequisite")
}

#[test]
fn a_timeline_projects_and_pulls_frames_at_the_project_resolution() {
    let mut b = backend();
    let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
    tl.props = props();
    b.set_timeline(&tl).unwrap();

    let f = b.frame_at(Frame(10), PreviewScale::Full).unwrap();
    assert_eq!(f.resolution(), props().resolution);
    assert!(f.is_well_formed());
    assert_eq!(f.position, Frame(10));
}

#[test]
fn a_scaled_pull_is_the_same_frame_at_a_smaller_size() {
    let mut b = backend();
    let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
    tl.props = props();
    b.set_timeline(&tl).unwrap();

    let full = b.frame_at(Frame(4), PreviewScale::Full).unwrap();
    let quarter = b.frame_at(Frame(4), PreviewScale::Quarter).unwrap();
    assert_eq!(
        quarter.resolution(),
        Resolution {
            width: 80,
            height: 44
        }
    );
    assert!(quarter.rgba.len() < full.rgba.len());
    // Scaling interpolates, so the signature is compared with a small
    // tolerance - the point is that it is the *same frame*, not that
    // resampling is bit-exact.
    let (a, b) = (full.signature(), quarter.signature());
    for c in 0..4 {
        assert!(
            a[c].abs_diff(b[c]) <= 8,
            "scaling must never change which frame came back: {a:?} vs {b:?}"
        );
    }
}

#[test]
fn offline_media_renders_as_a_placeholder_rather_than_failing() {
    let mut b = backend();
    let mut tl = davimci_core::testing::media_fixture(&[(0, 50, 0, 500)]);
    tl.props = props();
    let clip = clip_ids(&tl, "V1")[0];
    tl.set_media_offline(clip, true).unwrap();
    b.set_timeline(&tl).unwrap();

    let f = b.frame_at(Frame(1), PreviewScale::Full).unwrap();
    assert!(f.is_well_formed());
    assert_ne!(
        f.signature(),
        [0, 0, 0, 255],
        "the offline placeholder must be visible, not indistinguishable from a gap"
    );
}

#[test]
fn a_split_patches_the_playlist_instead_of_rebuilding_the_graph() {
    let mut b = backend();
    let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
    tl.props = props();
    b.set_timeline(&tl).unwrap();
    assert_eq!((b.rebuilds, b.patches), (1, 0));

    let v1 = track_id(&tl, "V1");
    tl.split_at(v1, Frame(40)).unwrap();
    b.set_timeline(&tl).unwrap();
    assert_eq!(
        (b.rebuilds, b.patches),
        (1, 1),
        "a split is a playlist mutation, not a rebuild (spec 10.1)"
    );

    // And the patched graph still plays.
    assert!(
        b.frame_at(Frame(45), PreviewScale::Full)
            .unwrap()
            .is_well_formed()
    );
}

#[test]
fn a_ripple_delete_patches_and_shortens_the_timeline() {
    let mut b = backend();
    let mut tl = fixture(&[("V1", &[(0, 50, "a"), (50, 50, "b"), (100, 50, "c")])]);
    tl.props = props();
    b.set_timeline(&tl).unwrap();
    let v1 = track_id(&tl, "V1");
    let mid = clip_ids(&tl, "V1")[1];
    tl.ripple_delete_clip(v1, mid).unwrap();
    b.set_timeline(&tl).unwrap();
    assert_eq!(b.rebuilds, 1);
    assert_eq!(b.patches, 1);
    let xml = b.to_xml().unwrap();
    assert_eq!(
        xml.matches("<entry ").count(),
        2,
        "the deleted entry must be gone from the projection"
    );
}

#[test]
fn adding_a_track_rebuilds_because_the_tractor_shape_changed() {
    let mut b = backend();
    let mut tl = fixture(&[("V1", &[(0, 10, "a")])]);
    tl.props = props();
    b.set_timeline(&tl).unwrap();
    tl.add_track(TrackKind::Audio);
    b.set_timeline(&tl).unwrap();
    assert_eq!(b.rebuilds, 2);
    assert_eq!(b.patches, 0);
}

#[test]
fn projecting_the_same_timeline_twice_touches_nothing() {
    let mut b = backend();
    let mut tl = fixture(&[("V1", &[(0, 10, "a")])]);
    tl.props = props();
    b.set_timeline(&tl).unwrap();
    b.set_timeline(&tl).unwrap();
    assert_eq!((b.rebuilds, b.patches), (1, 0));
}

#[test]
fn pulling_frames_without_a_timeline_is_an_error_not_a_panic() {
    let mut b = backend();
    assert!(b.frame_at(Frame(0), PreviewScale::Full).is_err());
    assert!(b.seek(Frame(0)).is_err());
}

#[test]
fn preview_calls_out_of_order_are_rejected() {
    let mut b = backend();
    assert!(!b.is_previewing());
    assert!(b.preview_stop().is_err());
    assert!(b.next_preview_frame().is_err());
}

#[test]
fn probing_missing_media_reports_it_offline() {
    let mut b = backend();
    let err = b
        .probe(std::path::Path::new("/definitely/not/here.mkv"))
        .unwrap_err();
    assert!(matches!(err, davimci_backend::BackendError::Offline { .. }));
}

/// Regression: playing to the end of the timeline leaves MLT's producer at
/// speed zero, and seeking back does not undo that - so the next play said
/// "playing" and never advanced a frame. Starting a preview resets the speed.
#[test]
fn a_preview_started_after_playback_ran_off_the_end_plays_again() {
    let mut b = backend();
    let tl = fixture(&[("V1", &[(0, 30, "a")])]);
    b.set_timeline(&tl).unwrap();
    // What reaching the end leaves behind.
    b.set_rate(0.0).unwrap();
    assert_eq!(b.playback_speed(), Some(0.0));

    b.preview_start(Frame(0), PreviewScale::Quarter).unwrap();
    assert_eq!(
        b.playback_speed(),
        Some(1.0),
        "a restarted preview is still paused at the producer"
    );
    b.preview_stop().unwrap();
}
