//! Golden view states, shared with every frontend's rendering tests
//!.
//!
//! A frontend renders these and diffs the result, so a view-state regression
//! fails here *and* in the frontend, and a frontend cannot quietly render
//! something the app never described.

use davimci_cmd::Session;
use davimci_core::testing::{fixture, track_id};
use davimci_core::{Frame, Timeline};
use davimci_keys::Mode;
use davimci_keys::mode::{Anchor, VisualSelection};
use davimci_motion::{JumpConfig, Zoom};

use crate::view::{ViewInputs, ViewState};
use crate::viewport::Viewport;

/// Two video tracks and one audio track with known cut positions.
#[must_use]
pub fn timeline() -> Timeline {
    fixture(&[
        ("V1", &[(0, 120, "a"), (120, 240, "b"), (400, 100, "c")]),
        ("V2", &[(60, 90, "over")]),
        ("A1", &[(0, 360, "music")]),
    ])
}

fn session() -> Session {
    Session::new(timeline())
}

fn viewport(zoom: Zoom) -> Viewport {
    let mut vp = Viewport::new(50, 3);
    vp.set_zoom(zoom, Frame::ZERO, Frame(500));
    vp
}

/// `NORMAL`, playhead at 0, fully zoomed in.
#[must_use]
pub fn normal() -> ViewState {
    let s = session();
    ViewState::build(
        &s,
        viewport(Zoom::new(8)),
        &JumpConfig::default(),
        &ViewInputs::default(),
    )
}

/// `NORMAL` with the playhead mid-timeline and the viewport followed to it.
#[must_use]
pub fn scrolled() -> ViewState {
    let mut s = session();
    let _ = s.set_playhead(Frame(300), track_id(s.timeline(), "V1"));
    let mut vp = viewport(Zoom::new(8));
    vp.follow_playhead(Frame(300), s.timeline().duration());
    ViewState::build(&s, vp, &JumpConfig::default(), &ViewInputs::default())
}

/// A `VISUAL` selection across two tracks - the widest selection description
/// the view state can carry.
#[must_use]
pub fn visual_across_tracks() -> ViewState {
    let mut s = session();
    let v1 = track_id(s.timeline(), "V1");
    let a1 = track_id(s.timeline(), "A1");
    let _ = s.set_playhead(Frame(100), v1);
    // Described through the same constructor the grammar drives, so the
    // fixture can only name a selection the key engine could also reach.
    let order: Vec<_> = s.timeline().tracks().iter().map(|t| t.id).collect();
    let sel = VisualSelection::spanning(
        Anchor {
            frame: Frame(60),
            track: v1,
        },
        davimci_motion::TimeRange::new(Frame(60), Frame(61)),
        Anchor {
            frame: Frame(240),
            track: a1,
        },
        davimci_motion::TimeRange::new(Frame(240), Frame(241)),
        &order,
    );
    let inputs = ViewInputs {
        mode: Mode::Visual,
        selection: Some(&sel),
        ..ViewInputs::default()
    };
    ViewState::build(&s, viewport(Zoom::new(8)), &JumpConfig::default(), &inputs)
}

/// Fully zoomed out: every clip collapses onto a handful of columns, which is
/// where quantisation bugs show up.
#[must_use]
pub fn zoomed_out() -> ViewState {
    let s = session();
    ViewState::build(
        &s,
        viewport(Zoom::OUT),
        &JumpConfig::default(),
        &ViewInputs::default(),
    )
}

/// An analysed audio lane: `A1` has a waveform, the video lanes do not
///. Frontends render this to prove the envelope reaches the
/// screen.
#[must_use]
pub fn waveform() -> ViewState {
    let s = session();
    let a1 = track_id(s.timeline(), "A1");
    let mut waves = crate::waveform::Waveforms::default();
    // A ramp from silence to full scale over the analysed span, so a
    // renderer that drops or reorders columns fails visibly.
    let peaks: Vec<f32> = (0..600u16)
        .map(|i| -60.0 + (f32::from(i) / 600.0) * 60.0)
        .collect();
    waves.insert(a1, crate::waveform::Waveform::from_db(10, &peaks));
    let inputs = ViewInputs {
        waveforms: Some(&waves),
        ..ViewInputs::default()
    };
    ViewState::build(&s, viewport(Zoom::new(8)), &JumpConfig::default(), &inputs)
}

/// Every golden view, with a stable name for snapshot files.
#[must_use]
pub fn all() -> Vec<(&'static str, ViewState)> {
    vec![
        ("normal", normal()),
        ("scrolled", scrolled()),
        ("visual_across_tracks", visual_across_tracks()),
        ("zoomed_out", zoomed_out()),
        ("waveform", waveform()),
    ]
}
