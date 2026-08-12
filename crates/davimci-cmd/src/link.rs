//! Link-group propagation.
//!
//! Imported video and audio arrive as one group, so an edit aimed at either
//! has to hit both. This module rewrites such an edit into the
//! [`EditCommand::Sequence`] that does it, before the edit is applied. The
//! expansion happens once, in [`crate::Session::exec`]: the log records the
//! expanded sequence, so undo, redo and replay never expand again and can
//! never expand twice.
//!
//! Only the edits that move a cut or a clip propagate. Property edits stay
//! where they were aimed - linked clips share timing, not gain or transform.

use davimci_core::{ClipId, Frame, GroupId, Timeline, TrackId};

use crate::command::EditCommand;

/// `cmd`, rewritten to reach every member of the group it touches.
///
/// Returns `cmd` unchanged when it touches no group, so an unlinked timeline
/// pays nothing and the log stays as small as what the user asked for.
pub(crate) fn expand(tl: &mut Timeline, cmd: &EditCommand) -> EditCommand {
    match cmd {
        EditCommand::Split {
            track,
            frame,
            new_id,
        } => split(tl, *track, *frame, *new_id),
        EditCommand::Join { track, frame } => join(tl, *track, *frame),
        EditCommand::MoveClip { track, clip, to } => {
            fan_out(tl, *track, *clip, |t, c| EditCommand::MoveClip {
                track: t,
                clip: c,
                to: *to,
            })
        }
        EditCommand::Trim {
            track,
            clip,
            edge,
            delta,
        } => fan_out(tl, *track, *clip, |t, c| EditCommand::Trim {
            track: t,
            clip: c,
            edge: *edge,
            delta: *delta,
        }),
        EditCommand::Slip { track, clip, delta } => {
            fan_out(tl, *track, *clip, |t, c| EditCommand::Slip {
                track: t,
                clip: c,
                delta: *delta,
            })
        }
        EditCommand::Slide { track, clip, delta } => {
            fan_out(tl, *track, *clip, |t, c| EditCommand::Slide {
                track: t,
                clip: c,
                delta: *delta,
            })
        }
        EditCommand::Roll { track, cut, delta } => {
            let Some(clip) = starting_at(tl, *track, *cut) else {
                return cmd.clone();
            };
            fan_out(tl, *track, clip, |t, _| EditCommand::Roll {
                track: t,
                cut: *cut,
                delta: *delta,
            })
        }
        EditCommand::Lift { track, start, end } => {
            range(tl, *track, *start, *end, cmd, &|t| EditCommand::Lift {
                track: t,
                start: *start,
                end: *end,
            })
        }
        EditCommand::RippleDelete { track, start, end } => {
            range(tl, *track, *start, *end, cmd, &|t| {
                EditCommand::RippleDelete {
                    track: t,
                    start: *start,
                    end: *end,
                }
            })
        }
        // A sequence expands element by element, so a macro or an import that
        // wraps a linked edit propagates like the bare edit would.
        EditCommand::Sequence(cmds) => {
            EditCommand::Sequence(cmds.iter().map(|c| expand(tl, c)).collect())
        }
        other => other.clone(),
    }
}

/// Members of the group `clip` belongs to, as `(track, clip)`, or `None` when
/// the clip is unlinked or is the only one left in its group.
fn members(tl: &Timeline, track: TrackId, clip: ClipId) -> Option<Vec<(TrackId, ClipId)>> {
    let (found, c) = tl.find_clip(clip)?;
    if found != track {
        return None;
    }
    let all = tl.group_members(c.group?);
    (all.len() > 1).then_some(all)
}

/// The clip starting exactly at `frame` on `track`.
fn starting_at(tl: &Timeline, track: TrackId, frame: Frame) -> Option<ClipId> {
    tl.track(track)?
        .clips()
        .iter()
        .find(|c| c.start == frame)
        .map(|c| c.id)
}

/// One command per group member, in track order.
fn fan_out(
    tl: &Timeline,
    track: TrackId,
    clip: ClipId,
    build: impl Fn(TrackId, ClipId) -> EditCommand,
) -> EditCommand {
    match members(tl, track, clip) {
        None => build(track, clip),
        Some(m) => EditCommand::Sequence(m.into_iter().map(|(t, c)| build(t, c)).collect()),
    }
}

/// A split of a grouped clip cuts every member at the same frame, and the
/// right-hand halves become a group of their own: the halves are no longer
/// the clip that was linked, but they are still each other's video and audio.
fn split(tl: &mut Timeline, track: TrackId, frame: Frame, new_id: Option<ClipId>) -> EditCommand {
    let plain = EditCommand::Split {
        track,
        frame,
        new_id,
    };
    let Some(clip) = tl.track(track).and_then(|t| t.clip_at(frame)) else {
        return plain;
    };
    if clip.start == frame {
        return plain;
    }
    let clip = clip.id;
    let Some(m) = members(tl, track, clip) else {
        return plain;
    };

    let mut halves = Vec::with_capacity(m.len());
    let mut cmds: Vec<EditCommand> = Vec::with_capacity(m.len() + 1);
    for (t, c) in m {
        let id = if c == clip {
            new_id.unwrap_or_else(|| tl.new_clip_id())
        } else {
            tl.new_clip_id()
        };
        halves.push(id);
        cmds.push(EditCommand::Split {
            track: t,
            frame,
            new_id: Some(id),
        });
    }
    let group = tl.new_group_id();
    cmds.push(EditCommand::Link {
        clips: halves,
        group: Some(group),
    });
    EditCommand::Sequence(cmds)
}

/// The inverse shape of [`split`]: the right-hand halves leave their group
/// first, because a linked clip may not be absorbed into its neighbour.
fn join(tl: &Timeline, track: TrackId, frame: Frame) -> EditCommand {
    let plain = EditCommand::Join { track, frame };
    let Some(clip) = starting_at(tl, track, frame) else {
        return plain;
    };
    let Some(m) = members(tl, track, clip) else {
        return plain;
    };
    let mut cmds: Vec<EditCommand> = m
        .iter()
        .map(|(_, c)| EditCommand::SetGroup {
            clip: *c,
            group: None,
        })
        .collect();
    cmds.extend(
        m.into_iter()
            .map(|(t, _)| EditCommand::Join { track: t, frame }),
    );
    EditCommand::Sequence(cmds)
}

/// A range edit repeats on every track a group in that range reaches, so
/// deleting a shot takes its audio with it.
fn range(
    tl: &mut Timeline,
    track: TrackId,
    start: Frame,
    end: Frame,
    cmd: &EditCommand,
    build: &dyn Fn(TrackId) -> EditCommand,
) -> EditCommand {
    let Some(t) = tl.track(track) else {
        return cmd.clone();
    };
    let groups: Vec<GroupId> = t
        .clips()
        .iter()
        .filter(|c| c.start < end && c.end() > start)
        .filter_map(|c| c.group)
        .collect();
    let mut tracks: Vec<TrackId> = vec![track];
    for g in groups {
        for (t, _) in tl.group_members(g) {
            if !tracks.contains(&t) {
                tracks.push(t);
            }
        }
    }
    if tracks.len() == 1 {
        return cmd.clone();
    }
    let mut cmds: Vec<EditCommand> = [start, end]
        .into_iter()
        .filter_map(|f| boundary_split(tl, &tracks, f))
        .collect();
    cmds.extend(tracks.into_iter().map(build));
    EditCommand::Sequence(cmds)
}

/// The cut a range edit needs at one of its boundaries, made on every track
/// the edit reaches at once so the halves left behind stay linked to each
/// other. Left to the per-track edit, each track would cut alone and the
/// survivors would come out ungrouped.
fn boundary_split(tl: &mut Timeline, tracks: &[TrackId], frame: Frame) -> Option<EditCommand> {
    let cut: Vec<TrackId> = tracks
        .iter()
        .copied()
        .filter(|t| tl.cuts_a_clip(*t, frame))
        .collect();
    if cut.is_empty() {
        return None;
    }
    let mut halves = Vec::with_capacity(cut.len());
    let mut cmds: Vec<EditCommand> = cut
        .into_iter()
        .map(|track| {
            let id = tl.new_clip_id();
            halves.push(id);
            EditCommand::Split {
                track,
                frame,
                new_id: Some(id),
            }
        })
        .collect();
    if halves.len() > 1 {
        let group = tl.new_group_id();
        cmds.push(EditCommand::Link {
            clips: halves,
            group: Some(group),
        });
    }
    Some(EditCommand::Sequence(cmds))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use crate::{EditCommand, Session};
    use davimci_core::testing::{fixture, track_id};
    use davimci_core::{ClipId, Edge, Frame, Timeline, TrackId};

    /// One 300-frame clip on V1, its linked twin on A1.
    fn linked() -> Session {
        let mut tl = fixture(&[("V1", &[(0, 300, "a")]), ("A1", &[(0, 300, "a")])]);
        let ids: Vec<ClipId> = tl
            .tracks()
            .iter()
            .filter_map(|t| t.clips().first().map(|c| c.id))
            .collect();
        tl.link(&ids).unwrap();
        Session::new(tl)
    }

    fn v1(s: &Session) -> TrackId {
        track_id(s.timeline(), "V1")
    }

    fn first_clip(tl: &Timeline, track: &str) -> ClipId {
        tl.track(track_id(tl, track)).unwrap().clips()[0].id
    }

    #[test]
    fn splitting_one_member_splits_the_whole_group() {
        let mut s = linked();
        s.exec(&EditCommand::Split {
            track: v1(&s),
            frame: Frame(100),
            new_id: None,
        })
        .unwrap();
        assert_eq!(
            s.timeline().dump(),
            "V1:[a 0-100 g5][a 100-300 g8]\nA1:[a 0-100 g5][a 100-300 g8]\n"
        );
        s.timeline().assert_invariants();
    }

    /// The halves stay each other's video and audio, in a group of their own:
    /// keeping the original group would leave it not frame-aligned.
    #[test]
    fn the_right_hand_halves_are_grouped_with_each_other() {
        let mut s = linked();
        s.exec(&EditCommand::Split {
            track: v1(&s),
            frame: Frame(100),
            new_id: None,
        })
        .unwrap();
        let groups: Vec<_> = s
            .timeline()
            .tracks()
            .iter()
            .map(|t| t.clips().iter().map(|c| c.group).collect::<Vec<_>>())
            .collect();
        assert_eq!(groups[0], groups[1], "video and audio share both groups");
        assert!(groups[0].iter().all(Option::is_some));
        assert_ne!(groups[0][0], groups[0][1], "the halves are not one group");
    }

    /// Regression: a backwards delete (`db`) cut inside the clip and left the
    /// surviving halves unlinked, while `dw` - which needed no cut - did not.
    #[test]
    fn a_partial_ripple_delete_keeps_the_survivors_linked() {
        let mut s = linked();
        let track = v1(&s);
        s.exec(&EditCommand::RippleDelete {
            track,
            start: Frame::ZERO,
            end: Frame(1),
        })
        .unwrap();
        let groups: Vec<_> = s
            .timeline()
            .tracks()
            .iter()
            .map(|t| t.clips().iter().map(|c| c.group).collect::<Vec<_>>())
            .collect();
        assert_eq!(groups[0], groups[1], "video and audio stay linked");
        assert!(
            groups[0].iter().all(Option::is_some),
            "survivors keep a group"
        );
        s.timeline().assert_invariants();
    }

    #[test]
    fn a_grouped_split_is_one_undoable_step() {
        let mut s = linked();
        let before = s.timeline().dump();
        s.exec(&EditCommand::Split {
            track: v1(&s),
            frame: Frame(100),
            new_id: None,
        })
        .unwrap();
        s.undo().unwrap();
        assert_eq!(s.timeline().dump(), before);
        s.redo().unwrap();
        assert_eq!(
            s.timeline().dump(),
            "V1:[a 0-100 g5][a 100-300 g8]\nA1:[a 0-100 g5][a 100-300 g8]\n"
        );
    }

    #[test]
    fn joining_undoes_a_grouped_split_from_the_key_side() {
        let mut s = linked();
        let track = v1(&s);
        s.exec(&EditCommand::Split {
            track,
            frame: Frame(100),
            new_id: None,
        })
        .unwrap();
        s.exec(&EditCommand::Join {
            track,
            frame: Frame(100),
        })
        .unwrap();
        assert_eq!(s.timeline().dump(), "V1:[a 0-300 g5]\nA1:[a 0-300 g5]\n");
        s.timeline().assert_invariants();
    }

    #[test]
    fn moving_one_member_moves_the_others() {
        let mut s = linked();
        let clip = first_clip(s.timeline(), "V1");
        s.exec(&EditCommand::MoveClip {
            track: v1(&s),
            clip,
            to: Frame(500),
        })
        .unwrap();
        assert_eq!(
            s.timeline().dump(),
            "V1:<gap 500>[a 500-800 g5]\nA1:<gap 500>[a 500-800 g5]\n"
        );
        s.timeline().assert_invariants();
    }

    #[test]
    fn trimming_one_member_trims_the_others() {
        let mut s = linked();
        let clip = first_clip(s.timeline(), "V1");
        s.exec(&EditCommand::Trim {
            track: v1(&s),
            clip,
            edge: Edge::Tail,
            delta: -50,
        })
        .unwrap();
        assert_eq!(s.timeline().dump(), "V1:[a 0-250 g5]\nA1:[a 0-250 g5]\n");
        s.timeline().assert_invariants();
    }

    #[test]
    fn deleting_a_range_takes_the_linked_audio_with_it() {
        let mut s = linked();
        s.exec(&EditCommand::RippleDelete {
            track: v1(&s),
            start: Frame(0),
            end: Frame(300),
        })
        .unwrap();
        assert_eq!(s.timeline().dump(), "V1: -\nA1: -\n");
    }

    /// A rejected grouped edit must leave the timeline and the id generator
    /// alone, even though expanding it reserved ids for the halves.
    #[test]
    fn a_rejected_grouped_split_hands_its_ids_back() {
        let mut s = linked();
        let cursor = s.timeline().id_cursor();
        let before = s.timeline().dump();
        assert!(
            s.exec(&EditCommand::Split {
                track: v1(&s),
                frame: Frame(0),
                new_id: None,
            })
            .is_err()
        );
        assert_eq!(s.timeline().dump(), before);
        assert_eq!(s.timeline().id_cursor(), cursor);
    }

    #[test]
    fn an_unlinked_edit_is_left_exactly_as_it_was_asked_for() {
        let mut tl = fixture(&[("V1", &[(0, 300, "a")]), ("A1", &[(0, 300, "a")])]);
        let cmd = EditCommand::Split {
            track: track_id(&tl, "V1"),
            frame: Frame(100),
            new_id: None,
        };
        let expanded = super::expand(&mut tl, &cmd);
        assert_eq!(expanded, cmd);
        let mut s = Session::new(tl);
        s.exec(&cmd).unwrap();
        assert_eq!(
            s.timeline().dump(),
            "V1:[a 0-100][a 100-300]\nA1:[a 0-300]\n"
        );
    }

    /// Ungrouping is `SetGroup`, and nothing follows the group afterwards.
    #[test]
    fn an_ungrouped_clip_moves_alone() {
        let mut s = linked();
        let video = first_clip(s.timeline(), "V1");
        let audio = first_clip(s.timeline(), "A1");
        s.exec(&EditCommand::Sequence(vec![
            EditCommand::SetGroup {
                clip: video,
                group: None,
            },
            EditCommand::SetGroup {
                clip: audio,
                group: None,
            },
        ]))
        .unwrap();
        s.exec(&EditCommand::MoveClip {
            track: v1(&s),
            clip: video,
            to: Frame(500),
        })
        .unwrap();
        assert_eq!(
            s.timeline().dump(),
            "V1:<gap 500>[a 500-800]\nA1:[a 0-300]\n"
        );
    }

    /// A macro or an import wraps its edits in a sequence, and a linked edit
    /// inside one propagates like the bare edit would.
    #[test]
    fn a_sequence_expands_element_by_element() {
        let mut s = linked();
        s.exec(&EditCommand::Sequence(vec![EditCommand::Split {
            track: v1(&s),
            frame: Frame(100),
            new_id: None,
        }]))
        .unwrap();
        assert_eq!(
            s.timeline().dump(),
            "V1:[a 0-100 g5][a 100-300 g8]\nA1:[a 0-100 g5][a 100-300 g8]\n"
        );
    }
}
