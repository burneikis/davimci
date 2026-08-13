//! Property tests for the command layer.
//!
//! The contract under test: applying then inverting a random command sequence
//! restores byte-identical serialized state, rejected commands never enter the
//! log, and redoing the log reproduces the state it recorded.

#![cfg(test)]
#![allow(clippy::unwrap_used)]

use proptest::prelude::*;

use davimci_core::testing::{clip_ids, media_fixture, track_id};
use davimci_core::{Classify, Clip, ClipId, Edge, ErrorClass, Frame, Timeline, TrackId};

use crate::command::EditCommand;
use crate::error::CmdError;
use crate::session::Session;

/// A command shape with positions drawn from a small range, so the generator
/// produces plenty of accepted commands as well as rejected ones.
#[derive(Debug, Clone)]
enum Shape {
    Split(u64),
    RippleDelete(u64, u64),
    Lift(u64, u64),
    Insert(u64, u64),
    Overwrite(u64, u64),
    Paste(u64, u64, u64),
    Move(usize, u64),
    Trim(usize, bool, i64),
    Roll(u64, i64),
    Slip(usize, i64),
    Slide(usize, i64),
    Join(u64),
}

fn shape() -> impl Strategy<Value = Shape> {
    prop_oneof![
        (0u64..400).prop_map(Shape::Split),
        (0u64..400, 1u64..120).prop_map(|(s, l)| Shape::RippleDelete(s, s + l)),
        (0u64..400, 1u64..120).prop_map(|(s, l)| Shape::Lift(s, s + l)),
        (0u64..400, 1u64..80).prop_map(|(at, l)| Shape::Insert(at, l)),
        (0u64..400, 1u64..80).prop_map(|(at, l)| Shape::Overwrite(at, l)),
        (0u64..400, 1u64..80, 0u64..400).prop_map(|(s, l, at)| Shape::Paste(s, s + l, at)),
        (0usize..6, 0u64..400).prop_map(|(i, at)| Shape::Move(i, at)),
        (0usize..6, any::<bool>(), -60i64..60).prop_map(|(i, h, d)| Shape::Trim(i, h, d)),
        (0u64..400, -60i64..60).prop_map(|(c, d)| Shape::Roll(c, d)),
        (0usize..6, -60i64..60).prop_map(|(i, d)| Shape::Slip(i, d)),
        (0usize..6, -60i64..60).prop_map(|(i, d)| Shape::Slide(i, d)),
        (0u64..400).prop_map(Shape::Join),
    ]
}

fn base() -> Timeline {
    media_fixture(&[
        (0, 100, 50, 400),
        (100, 80, 50, 400),
        (180, 120, 50, 400),
        (340, 60, 50, 400),
    ])
}

/// Turn a shape into a command against the current state.
fn build(tl: &Timeline, v1: TrackId, shape: &Shape) -> EditCommand {
    let ids = clip_ids(tl, "V1");
    let pick = |i: usize| ids.get(i % ids.len().max(1)).copied().unwrap_or(ClipId(0));
    match *shape {
        Shape::Split(f) => EditCommand::Split {
            track: v1,
            frame: Frame(f),
            new_id: None,
        },
        Shape::RippleDelete(a, b) => EditCommand::RippleDelete {
            track: v1,
            start: Frame(a),
            end: Frame(b),
        },
        Shape::Lift(a, b) => EditCommand::Lift {
            track: v1,
            start: Frame(a),
            end: Frame(b),
        },
        Shape::Insert(at, len) => EditCommand::Insert {
            track: v1,
            at: Frame(at),
            clip: Clip::generated(ClipId(0), "n", Frame::ZERO, Frame(len)),
            new_id: None,
        },
        Shape::Overwrite(at, len) => EditCommand::Overwrite {
            track: v1,
            at: Frame(at),
            clip: Clip::generated(ClipId(0), "n", Frame::ZERO, Frame(len)),
            new_id: None,
        },
        Shape::Paste(a, b, at) => EditCommand::Paste {
            track: v1,
            at: Frame(at),
            register: tl.yank_range(v1, Frame(a), Frame(b)).unwrap_or_default(),
            ripple: true,
        },
        Shape::Move(i, at) => EditCommand::MoveClip {
            track: v1,
            clip: pick(i),
            to: Frame(at),
        },
        Shape::Trim(i, head, d) => EditCommand::Trim {
            track: v1,
            clip: pick(i),
            edge: if head { Edge::Head } else { Edge::Tail },
            delta: d,
        },
        Shape::Roll(cut, d) => EditCommand::Roll {
            track: v1,
            cut: Frame(cut),
            delta: d,
        },
        Shape::Slip(i, d) => EditCommand::Slip {
            track: v1,
            clip: pick(i),
            delta: d,
        },
        Shape::Slide(i, d) => EditCommand::Slide {
            track: v1,
            clip: pick(i),
            delta: d,
        },
        Shape::Join(f) => EditCommand::Join {
            track: v1,
            frame: Frame(f),
        },
    }
}

fn json(tl: &Timeline) -> String {
    serde_json::to_string(tl).unwrap_or_default()
}

/// Execute a shape, asserting the rejection policy. Returns whether it ran.
fn exec(session: &mut Session, v1: TrackId, shape: &Shape) -> Result<bool, TestCaseError> {
    let cmd = build(session.timeline(), v1, shape);
    let before = json(session.timeline());
    match session.exec(&cmd) {
        Ok(_) => {
            session
                .timeline()
                .check_invariants()
                .map_err(|e| TestCaseError::fail(e.to_string()))?;
            Ok(true)
        }
        Err(e) => {
            // Ordinary input must never produce a corruption-class error, and
            // a rejection must leave the state byte-identical.
            prop_assert_eq!(e.class(), ErrorClass::User, "{:?} gave {}", shape, e);
            prop_assert!(!e.user_message().is_empty());
            prop_assert_eq!(json(session.timeline()), before, "mutated on {:?}", shape);
            Ok(false)
        }
    }
}

proptest! {
    /// The headline undo property: undoing everything gets the project
    /// back exactly, ids and all, however the edits interleaved.
    #[test]
    fn undoing_everything_restores_byte_identical_state(
        shapes in prop::collection::vec(shape(), 1..30)
    ) {
        let mut session = Session::new(base());
        let v1 = track_id(session.timeline(), "V1");
        let start = json(session.timeline());

        let mut applied = 0;
        for s in &shapes {
            if exec(&mut session, v1, s)? {
                applied += 1;
            }
        }
        for _ in 0..applied {
            session.undo().map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
        prop_assert_eq!(json(session.timeline()), start);
        prop_assert_eq!(session.undo(), Err(CmdError::NothingToUndo));
    }

    /// And redoing the whole log reproduces the state it recorded.
    #[test]
    fn redoing_everything_reproduces_the_final_state(
        shapes in prop::collection::vec(shape(), 1..30)
    ) {
        let mut session = Session::new(base());
        let v1 = track_id(session.timeline(), "V1");

        let mut applied = 0;
        for s in &shapes {
            if exec(&mut session, v1, s)? {
                applied += 1;
            }
        }
        let end = json(session.timeline());
        for _ in 0..applied {
            session.undo().map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
        for _ in 0..applied {
            session.redo().map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
        prop_assert_eq!(json(session.timeline()), end);
    }

    /// Snapshots are an optimisation, never a semantic change: the same
    /// script must end in the same state at any drift-guard interval.
    #[test]
    fn the_snapshot_interval_does_not_change_behaviour(
        shapes in prop::collection::vec(shape(), 1..20),
        interval in 0u64..5,
    ) {
        let mut a = Session::new(base());
        let mut b = Session::new(base());
        b.set_snapshot_interval(interval);
        let v1 = track_id(a.timeline(), "V1");

        let mut applied = 0;
        for s in &shapes {
            let ran = exec(&mut a, v1, s)?;
            prop_assert_eq!(ran, exec(&mut b, v1, s)?);
            if ran {
                applied += 1;
            }
        }
        prop_assert_eq!(json(a.timeline()), json(b.timeline()));
        for _ in 0..applied {
            a.undo().map_err(|e| TestCaseError::fail(e.to_string()))?;
            b.undo().map_err(|e| TestCaseError::fail(e.to_string()))?;
        }
        prop_assert_eq!(json(a.timeline()), json(b.timeline()));
    }

    /// A saved project reloads to exactly the state that was saved.
    #[test]
    fn saving_and_reloading_is_lossless(shapes in prop::collection::vec(shape(), 1..20)) {
        let mut session = Session::new(base());
        let v1 = track_id(session.timeline(), "V1");
        for s in &shapes {
            exec(&mut session, v1, s)?;
        }
        let text = crate::project::ProjectFile::from_session(&session)
            .to_json()
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        let reloaded = crate::project::ProjectFile::from_json(&text)
            .and_then(crate::project::ProjectFile::into_timeline)
            .map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(json(&reloaded), json(session.timeline()));
    }
}
