//! Serializable edit commands (plan.md Phase 2, spec §10.4).
//!
//! Every mutation of a [`Timeline`] outside `davimci-core`'s own tests goes
//! through one of these. A command applies, and reports the command that
//! undoes it - undo, `.`-repeat, macros, the Lua API, and the project format
//! are all built on that one pair.
//!
//! ## Why `apply` returns the inverse
//!
//! plan.md originally sketched `fn invert(&self) -> Box<dyn Command>`. That
//! signature cannot work: the inverse of a ripple delete is "put *these*
//! clips back", which is only known once the delete has run. So the inverse
//! is produced by [`Command::apply`] as part of its [`Effect`], and plan.md
//! has been amended to match.
//!
//! ## Determinism
//!
//! Redoing a command must reproduce byte-identical state, ids included, so no
//! command may mint an id the log does not record. Commands that would cause
//! an incidental cut - inserting into the middle of a clip, deleting a
//! part-range - therefore *expand* into a [`EditCommand::Sequence`] with an
//! explicit [`EditCommand::Split`] in front, and [`Effect::applied`] carries
//! that expansion. Joining those cuts back up on undo then falls out of
//! inverting the sequence.

use serde::{Deserialize, Serialize};

use davimci_core::{
    Clip, ClipId, ClipProps, ConformState, CoreError, Edge, Frame, GroupId, Register,
    TimelineProps, TrackId,
};
use davimci_core::{Timeline, TrackKind};

use crate::error::CmdError;

/// What applying a command did.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    /// The command as actually executed, with every id pinned. Replaying this
    /// reproduces the same state exactly; this is what the log stores.
    pub applied: EditCommand,
    /// The command that undoes it.
    pub inverse: EditCommand,
}

/// The one write path into a timeline.
pub trait Command {
    /// Apply to `tl`, returning the effect. Validate-then-mutate: on `Err`
    /// the timeline is byte-identical to what it was.
    fn apply(&self, tl: &mut Timeline) -> Result<Effect, CmdError>;
    /// One-line description for `:undolist` and the status line.
    fn describe(&self) -> String;
}

/// Every edit davimci can perform.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EditCommand {
    /// Cut the clip under `frame` in two (spec §4, `s`).
    Split {
        track: TrackId,
        frame: Frame,
        /// `None` mints a fresh id; the log always records `Some`.
        new_id: Option<ClipId>,
    },
    /// Merge the two halves of a split back together.
    Join { track: TrackId, frame: Frame },
    /// Remove a range, leaving a gap (spec §4, `gd`).
    Lift {
        track: TrackId,
        start: Frame,
        end: Frame,
    },
    /// Remove a range and close the gap (spec §4, `x`/`d`).
    RippleDelete {
        track: TrackId,
        start: Frame,
        end: Frame,
    },
    /// Insert a clip, rippling later clips right (spec §4, `i`).
    Insert {
        track: TrackId,
        at: Frame,
        clip: Clip,
        new_id: Option<ClipId>,
    },
    /// Place a clip over whatever is there (spec §4, `gp`).
    Overwrite {
        track: TrackId,
        at: Frame,
        clip: Clip,
        new_id: Option<ClipId>,
    },
    /// Paste register contents, minting fresh clips (spec §4, `p`).
    Paste {
        track: TrackId,
        at: Frame,
        register: Register,
        ripple: bool,
    },
    /// Put previously removed clips back, ids and linkage intact. This is the
    /// id-preserving form every paste and delete materialises into.
    Restore {
        track: TrackId,
        at: Frame,
        clips: Vec<Clip>,
        span: Frame,
        ripple: bool,
    },
    /// Move a clip elsewhere on its track.
    MoveClip {
        track: TrackId,
        clip: ClipId,
        to: Frame,
    },
    /// Ripple trim one edge (spec §4.0.1, `t`).
    Trim {
        track: TrackId,
        clip: ClipId,
        edge: Edge,
        delta: i64,
    },
    /// Roll a cut point (spec §4.0.1, `gt`).
    Roll {
        track: TrackId,
        cut: Frame,
        delta: i64,
    },
    /// Slip a clip's source window (spec §4.0.1, `T`).
    Slip {
        track: TrackId,
        clip: ClipId,
        delta: i64,
    },
    /// Slide a clip between its neighbours (spec §4.0.1, `gT`).
    Slide {
        track: TrackId,
        clip: ClipId,
        delta: i64,
    },
    /// Link clips into one group (spec §5).
    Link {
        clips: Vec<ClipId>,
        group: Option<GroupId>,
    },
    /// Put one clip into a group, or take it out (`:unlink`).
    SetGroup {
        clip: ClipId,
        group: Option<GroupId>,
    },
    /// Add a track (spec §7 import: one track per stream).
    AddTrack {
        kind: TrackKind,
        /// `None` takes the next name in the `V1`/`A2` sequence.
        name: Option<String>,
        /// `None` mints a fresh id; the log always records `Some`.
        new_id: Option<TrackId>,
    },
    /// Remove an empty track.
    RemoveTrack { track: TrackId },
    /// Change the timeline's framerate/resolution, retiming every clip
    /// (spec §7.1). One undoable command, however many clips it moves.
    Reconform { props: TimelineProps },
    /// Put back a geometry captured by a re-conform. This, not another
    /// `Reconform`, is a re-conform's inverse: rounding is not reversible,
    /// so undo replays the exact prior state instead of recomputing it.
    RestoreConform { state: Box<ConformState> },
    /// Replace a clip's non-destructive properties (spec §6.1, §8).
    SetProps {
        track: TrackId,
        clip: ClipId,
        props: ClipProps,
    },
    /// Several commands as one undo step. Rolls back completely on failure.
    Sequence(Vec<EditCommand>),
}

/// Names of every variant. Kept honest by the exhaustive match in
/// [`EditCommand::variant_name`]: a new variant will not compile until it is
/// named, and the serialization test asserts every name has a sample.
pub const VARIANT_NAMES: &[&str] = &[
    "Split",
    "Join",
    "Lift",
    "RippleDelete",
    "Insert",
    "Overwrite",
    "Paste",
    "Restore",
    "MoveClip",
    "Trim",
    "Roll",
    "Slip",
    "Slide",
    "Link",
    "SetGroup",
    "AddTrack",
    "RemoveTrack",
    "Reconform",
    "RestoreConform",
    "SetProps",
    "Sequence",
];

impl EditCommand {
    #[must_use]
    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Split { .. } => "Split",
            Self::Join { .. } => "Join",
            Self::Lift { .. } => "Lift",
            Self::RippleDelete { .. } => "RippleDelete",
            Self::Insert { .. } => "Insert",
            Self::Overwrite { .. } => "Overwrite",
            Self::Paste { .. } => "Paste",
            Self::Restore { .. } => "Restore",
            Self::MoveClip { .. } => "MoveClip",
            Self::Trim { .. } => "Trim",
            Self::Roll { .. } => "Roll",
            Self::Slip { .. } => "Slip",
            Self::Slide { .. } => "Slide",
            Self::Link { .. } => "Link",
            Self::SetGroup { .. } => "SetGroup",
            Self::AddTrack { .. } => "AddTrack",
            Self::RemoveTrack { .. } => "RemoveTrack",
            Self::Reconform { .. } => "Reconform",
            Self::RestoreConform { .. } => "RestoreConform",
            Self::SetProps { .. } => "SetProps",
            Self::Sequence(_) => "Sequence",
        }
    }

    /// The form used by `.`-repeat: pinned ids are dropped so a repeat mints
    /// new clips instead of colliding with the ones it created last time.
    #[must_use]
    pub fn for_repeat(&self) -> Self {
        match self {
            Self::Split { track, frame, .. } => Self::Split {
                track: *track,
                frame: *frame,
                new_id: None,
            },
            Self::Insert {
                track, at, clip, ..
            } => Self::Insert {
                track: *track,
                at: *at,
                clip: clip.clone(),
                new_id: None,
            },
            Self::Overwrite {
                track, at, clip, ..
            } => Self::Overwrite {
                track: *track,
                at: *at,
                clip: clip.clone(),
                new_id: None,
            },
            Self::Restore {
                track,
                at,
                clips,
                span,
                ripple,
            } => Self::Paste {
                track: *track,
                at: *at,
                register: Register {
                    clips: clips.clone(),
                    span: *span,
                },
                ripple: *ripple,
            },
            Self::Link { clips, .. } => Self::Link {
                clips: clips.clone(),
                group: None,
            },
            Self::AddTrack { kind, .. } => Self::AddTrack {
                kind: *kind,
                // A repeated import makes another track, not a name clash.
                name: None,
                new_id: None,
            },
            Self::Sequence(v) => Self::Sequence(v.iter().map(Self::for_repeat).collect()),
            other => other.clone(),
        }
    }
}

impl Command for EditCommand {
    fn describe(&self) -> String {
        match self {
            Self::Split { frame, .. } => format!("split at {frame}"),
            Self::Join { frame, .. } => format!("join at {frame}"),
            Self::Lift { start, end, .. } => format!("lift {start}-{end}"),
            Self::RippleDelete { start, end, .. } => format!("ripple delete {start}-{end}"),
            Self::Insert { at, clip, .. } => format!("insert {} at {at}", clip.label),
            Self::Overwrite { at, clip, .. } => format!("overwrite {} at {at}", clip.label),
            Self::Paste { at, .. } => format!("paste at {at}"),
            Self::Restore { at, clips, .. } => format!("restore {} clips at {at}", clips.len()),
            Self::MoveClip { clip, to, .. } => format!("move {clip} to {to}"),
            Self::Trim {
                clip, edge, delta, ..
            } => format!("trim {clip} {edge:?} by {delta}"),
            Self::Roll { cut, delta, .. } => format!("roll cut at {cut} by {delta}"),
            Self::Slip { clip, delta, .. } => format!("slip {clip} by {delta}"),
            Self::Slide { clip, delta, .. } => format!("slide {clip} by {delta}"),
            Self::Link { clips, .. } => format!("link {} clips", clips.len()),
            Self::SetGroup { clip, group: None } => format!("unlink {clip}"),
            Self::SetGroup {
                clip,
                group: Some(g),
            } => format!("put {clip} in {g}"),
            Self::AddTrack { kind, name, .. } => match name {
                Some(n) => format!("add track {n}"),
                None => format!("add a {} track", kind.prefix()),
            },
            Self::RemoveTrack { track } => format!("remove track {track}"),
            Self::Reconform { props } => format!("conform the timeline to {props}"),
            Self::RestoreConform { state } => {
                format!("conform the timeline back to {}", state.props)
            }
            Self::SetProps { clip, .. } => format!("set properties of {clip}"),
            Self::Sequence(v) => match v.len() {
                0 => "nothing".to_string(),
                1 => v.iter().map(Command::describe).collect(),
                n => format!("{n} edits"),
            },
        }
    }

    fn apply(&self, tl: &mut Timeline) -> Result<Effect, CmdError> {
        match self {
            Self::Split {
                track,
                frame,
                new_id,
            } => {
                let id = match new_id {
                    Some(id) => tl.split_at_with_id(*track, *frame, *id)?,
                    None => with_ids(tl, 1, |tl, ids| {
                        let id = first(ids)?;
                        Ok(tl.split_at_with_id(*track, *frame, id)?)
                    })?,
                };
                Ok(Effect {
                    applied: Self::Split {
                        track: *track,
                        frame: *frame,
                        new_id: Some(id),
                    },
                    inverse: Self::Join {
                        track: *track,
                        frame: *frame,
                    },
                })
            }

            Self::Join { track, frame } => {
                let absorbed = tl.join_at(*track, *frame)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Split {
                        track: *track,
                        frame: *frame,
                        new_id: Some(absorbed),
                    },
                })
            }

            Self::Lift { track, start, end } => {
                if let Some(expanded) = expand_cuts(tl, *track, &[*start, *end], self) {
                    return expanded.apply(tl);
                }
                let reg = tl.lift_range(*track, *start, *end)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Restore {
                        track: *track,
                        at: *start,
                        clips: reg.clips,
                        span: reg.span,
                        ripple: false,
                    },
                })
            }

            Self::RippleDelete { track, start, end } => {
                if let Some(expanded) = expand_cuts(tl, *track, &[*start, *end], self) {
                    return expanded.apply(tl);
                }
                let reg = tl.ripple_delete_range(*track, *start, *end)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Restore {
                        track: *track,
                        at: *start,
                        clips: reg.clips,
                        span: reg.span,
                        ripple: true,
                    },
                })
            }

            Self::Restore {
                track,
                at,
                clips,
                span,
                ripple,
            } => {
                let end = plus(*at, *span);
                let cuts: &[Frame] = if *ripple { &[*at] } else { &[*at, end] };
                if let Some(expanded) = expand_cuts(tl, *track, cuts, self) {
                    return expanded.apply(tl);
                }
                let inverse = if *ripple {
                    Self::RippleDelete {
                        track: *track,
                        start: *at,
                        end,
                    }
                } else {
                    // Whatever we are about to cover has to come back.
                    let covered = tl.yank_range(*track, *at, end)?;
                    let lift = Self::Lift {
                        track: *track,
                        start: *at,
                        end,
                    };
                    if covered.is_empty() {
                        lift
                    } else {
                        Self::Sequence(vec![
                            lift,
                            Self::Restore {
                                track: *track,
                                at: *at,
                                clips: covered.clips,
                                span: covered.span,
                                ripple: false,
                            },
                        ])
                    }
                };
                tl.restore(*track, *at, clips, *span, *ripple)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse,
                })
            }

            Self::Insert {
                track,
                at,
                clip,
                new_id,
            } => place(tl, *track, *at, clip, *new_id, true),

            Self::Overwrite {
                track,
                at,
                clip,
                new_id,
            } => place(tl, *track, *at, clip, *new_id, false),

            Self::Paste {
                track,
                at,
                register,
                ripple,
            } => {
                if register.is_empty() {
                    return Err(CoreError::EmptyRegister.into());
                }
                with_ids(tl, register.clips.len(), |tl, ids| {
                    let clips = register
                        .clips
                        .iter()
                        .zip(ids)
                        .map(|(c, id)| {
                            let mut copy = c.clone();
                            copy.id = *id;
                            // A paste is new material: it inherits no linkage.
                            copy.group = None;
                            copy
                        })
                        .collect();
                    Self::Restore {
                        track: *track,
                        at: *at,
                        clips,
                        span: register.span,
                        ripple: *ripple,
                    }
                    .apply(tl)
                })
            }

            Self::MoveClip { track, clip, to } => {
                let (found, c) = tl
                    .find_clip(*clip)
                    .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?;
                if found != *track {
                    return Err(CoreError::NoSuchClip(clip.to_string()).into());
                }
                let (start, end, dur) = (c.start, c.end(), c.duration);
                let mut moved = c.clone();
                moved.start = Frame::ZERO;
                Self::Sequence(vec![
                    Self::Lift {
                        track: *track,
                        start,
                        end,
                    },
                    Self::Restore {
                        track: *track,
                        at: *to,
                        clips: vec![moved],
                        span: dur,
                        ripple: false,
                    },
                ])
                .apply(tl)
            }

            Self::Trim {
                track,
                clip,
                edge,
                delta,
            } => {
                let back = negate(*delta)?;
                tl.ripple_trim(*track, *clip, *edge, *delta)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Trim {
                        track: *track,
                        clip: *clip,
                        edge: *edge,
                        delta: back,
                    },
                })
            }

            Self::Roll { track, cut, delta } => {
                let back = negate(*delta)?;
                tl.roll(*track, *cut, *delta)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Roll {
                        track: *track,
                        cut: plus_signed(*cut, *delta)?,
                        delta: back,
                    },
                })
            }

            Self::Slip { track, clip, delta } => {
                let back = negate(*delta)?;
                tl.slip(*track, *clip, *delta)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Slip {
                        track: *track,
                        clip: *clip,
                        delta: back,
                    },
                })
            }

            Self::Slide { track, clip, delta } => {
                let back = negate(*delta)?;
                tl.slide(*track, *clip, *delta)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::Slide {
                        track: *track,
                        clip: *clip,
                        delta: back,
                    },
                })
            }

            Self::Link { clips, group } => {
                if clips.len() < 2 {
                    return Err(CoreError::CannotLink {
                        reason: "a link group needs at least two clips".into(),
                    }
                    .into());
                }
                let reserved = tl.id_cursor();
                let group = match group {
                    Some(g) => *g,
                    None => tl.new_group_id(),
                };
                let members = clips
                    .iter()
                    .map(|c| Self::SetGroup {
                        clip: *c,
                        group: Some(group),
                    })
                    .collect();
                match Self::Sequence(members).apply(tl) {
                    Ok(effect) => Ok(Effect {
                        applied: Self::Link {
                            clips: clips.clone(),
                            group: Some(group),
                        },
                        inverse: effect.inverse,
                    }),
                    Err(e) => {
                        tl.set_id_cursor(reserved);
                        Err(e)
                    }
                }
            }

            Self::SetGroup { clip, group } => {
                let previous = tl
                    .find_clip(*clip)
                    .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?
                    .1
                    .group;
                tl.set_group(*clip, *group)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::SetGroup {
                        clip: *clip,
                        group: previous,
                    },
                })
            }

            Self::AddTrack { kind, name, new_id } => {
                let name = name.clone().unwrap_or_else(|| tl.next_track_name(*kind));
                let cursor = tl.id_cursor();
                let id = match new_id {
                    Some(id) => *id,
                    None => tl.new_track_id(),
                };
                match tl.add_track_with_id(id, name.clone(), *kind) {
                    Ok(()) => Ok(Effect {
                        applied: Self::AddTrack {
                            kind: *kind,
                            name: Some(name),
                            new_id: Some(id),
                        },
                        inverse: Self::RemoveTrack { track: id },
                    }),
                    Err(e) => {
                        tl.set_id_cursor(cursor);
                        Err(e.into())
                    }
                }
            }

            Self::RemoveTrack { track } => {
                let (name, kind) = tl.remove_track(*track)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::AddTrack {
                        kind,
                        name: Some(name),
                        new_id: Some(*track),
                    },
                })
            }

            Self::Reconform { props } => {
                let before = tl.reconform(*props)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::RestoreConform {
                        state: Box::new(before),
                    },
                })
            }

            Self::RestoreConform { state } => {
                let before = tl.restore_conform(state)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::RestoreConform {
                        state: Box::new(before),
                    },
                })
            }

            Self::SetProps {
                track,
                clip,
                props: next,
            } => {
                let previous = tl
                    .find_clip(*clip)
                    .ok_or_else(|| CoreError::NoSuchClip(clip.to_string()))?
                    .1
                    .props;
                tl.set_clip_props(*track, *clip, *next)?;
                Ok(Effect {
                    applied: self.clone(),
                    inverse: Self::SetProps {
                        track: *track,
                        clip: *clip,
                        props: previous,
                    },
                })
            }

            Self::Sequence(cmds) => {
                let (applied, inverse) = apply_all(tl, cmds)?;
                Ok(Effect {
                    applied: Self::Sequence(applied),
                    inverse: Self::Sequence(inverse),
                })
            }
        }
    }
}

// -- helpers -------------------------------------------------------------

fn plus(a: Frame, b: Frame) -> Frame {
    Frame(a.get().saturating_add(b.get()))
}

fn plus_signed(a: Frame, delta: i64) -> Result<Frame, CmdError> {
    let v = i128::from(a.get()) + i128::from(delta);
    if v < 0 {
        return Err(CoreError::NegativeTime.into());
    }
    Ok(Frame(v.max(0) as u64))
}

fn negate(delta: i64) -> Result<i64, CmdError> {
    // An un-negatable delta could never be undone, so it is rejected here
    // rather than after the timeline has moved.
    delta
        .checked_neg()
        .ok_or(CmdError::Core(CoreError::NegativeTime))
}

fn first(ids: &[ClipId]) -> Result<ClipId, CmdError> {
    ids.first()
        .copied()
        .ok_or_else(|| CmdError::ReplayFailed("no clip id was reserved".into()))
}

/// Reserve `n` clip ids, run `f`, and hand them back if it fails - so a
/// rejected command leaves even the id generator untouched.
fn with_ids<T>(
    tl: &mut Timeline,
    n: usize,
    f: impl FnOnce(&mut Timeline, &[ClipId]) -> Result<T, CmdError>,
) -> Result<T, CmdError> {
    let cursor = tl.id_cursor();
    let ids: Vec<ClipId> = (0..n).map(|_| tl.new_clip_id()).collect();
    match f(tl, &ids) {
        Ok(v) => Ok(v),
        Err(e) => {
            tl.set_id_cursor(cursor);
            Err(e)
        }
    }
}

/// If `cmd` would cut a clip at any of `frames`, rewrite it as an explicit
/// split sequence so the log records the ids those cuts create.
fn expand_cuts(
    tl: &Timeline,
    track: TrackId,
    frames: &[Frame],
    cmd: &EditCommand,
) -> Option<EditCommand> {
    let mut out: Vec<EditCommand> = frames
        .iter()
        .filter(|f| tl.cuts_a_clip(track, **f))
        .map(|f| EditCommand::Split {
            track,
            frame: *f,
            new_id: None,
        })
        .collect();
    if out.is_empty() {
        return None;
    }
    out.push(cmd.clone());
    Some(EditCommand::Sequence(out))
}

/// Shared body of `Insert` (ripple) and `Overwrite`.
fn place(
    tl: &mut Timeline,
    track: TrackId,
    at: Frame,
    clip: &Clip,
    new_id: Option<ClipId>,
    ripple: bool,
) -> Result<Effect, CmdError> {
    let n = usize::from(new_id.is_none());
    with_ids(tl, n, |tl, ids| {
        let mut c = clip.clone();
        c.id = match new_id {
            Some(id) => id,
            None => first(ids)?,
        };
        c.start = Frame::ZERO;
        let span = c.duration;
        EditCommand::Restore {
            track,
            at,
            clips: vec![c],
            span,
            ripple,
        }
        .apply(tl)
    })
}

/// Apply commands in order, rolling back completely if one is rejected.
fn apply_all(
    tl: &mut Timeline,
    cmds: &[EditCommand],
) -> Result<(Vec<EditCommand>, Vec<EditCommand>), CmdError> {
    let cursor = tl.id_cursor();
    let mut applied: Vec<EditCommand> = Vec::new();
    let mut inverses: Vec<EditCommand> = Vec::new();
    for c in cmds {
        match c.apply(tl) {
            Ok(e) => {
                push_flat(&mut applied, e.applied);
                inverses.push(e.inverse);
            }
            Err(err) => {
                for inv in inverses.iter().rev() {
                    if let Err(bad) = inv.apply(tl) {
                        return Err(CmdError::ReplayFailed(format!(
                            "could not undo a partly applied sequence: {bad}"
                        )));
                    }
                }
                // A rejected sequence hands back the ids its applied steps
                // minted, so the timeline is byte-identical, generator too.
                tl.set_id_cursor(cursor);
                return Err(err);
            }
        }
    }
    let mut flat = Vec::new();
    for inv in inverses.into_iter().rev() {
        push_flat(&mut flat, inv);
    }
    Ok((applied, flat))
}

fn push_flat(out: &mut Vec<EditCommand>, cmd: EditCommand) {
    match cmd {
        EditCommand::Sequence(v) => out.extend(v),
        other => out.push(other),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use davimci_core::testing::{clip_ids, fixture, media_fixture, track_id};
    use davimci_core::{ClipProps, MediaRef, TimelineProps};

    /// Apply, undo, and redo `cmd`, asserting the two properties the whole
    /// phase rests on: undo restores byte-identical state, and redo of the
    /// materialised command reproduces the post-apply state exactly.
    ///
    /// The id cursor is restored around each step exactly as [`UndoTree`]
    /// does, since which ids are still unspent is history's business, not the
    /// command's - see `undo::UndoTree::reconcile`.
    fn roundtrip(tl: &mut Timeline, cmd: &EditCommand) -> Effect {
        let cursor_before = tl.id_cursor();
        let before = json(tl);
        let effect = cmd.apply(tl).unwrap();
        let cursor_after = tl.id_cursor();
        let after = json(tl);
        tl.assert_invariants();

        effect.inverse.apply(tl).unwrap();
        tl.set_id_cursor(cursor_before);
        assert_eq!(json(tl), before, "undo of {} drifted", cmd.describe());

        effect.applied.apply(tl).unwrap();
        tl.set_id_cursor(cursor_after);
        assert_eq!(json(tl), after, "redo of {} drifted", cmd.describe());
        effect
    }

    fn json(tl: &Timeline) -> String {
        serde_json::to_string(tl).unwrap()
    }

    fn sample_clip() -> Clip {
        Clip::generated(ClipId(0), "n", Frame::ZERO, Frame(30))
    }

    #[test]
    fn split_and_join_are_inverses() {
        let mut tl = media_fixture(&[(0, 100, 20, 300)]);
        let v1 = track_id(&tl, "V1");
        let effect = roundtrip(
            &mut tl,
            &EditCommand::Split {
                track: v1,
                frame: Frame(40),
                new_id: None,
            },
        );
        // The log always pins the id the split created.
        assert!(matches!(
            effect.applied,
            EditCommand::Split {
                new_id: Some(_),
                ..
            }
        ));
        assert!(matches!(effect.inverse, EditCommand::Join { .. }));
    }

    #[test]
    fn ripple_delete_of_a_part_range_rejoins_on_undo() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let effect = roundtrip(
            &mut tl,
            &EditCommand::RippleDelete {
                track: v1,
                start: Frame(40),
                end: Frame(60),
            },
        );
        // The remaining halves are discontinuous in the source, so the cut
        // at 40 stays; only the deleted 20 frames are gone.
        assert_eq!(tl.dump(), "V1:[a 0-40][a 40-80]\nA1: -\n");
        // Two cuts were needed, so the log records both splits.
        let EditCommand::Sequence(steps) = &effect.applied else {
            unreachable!("a part-range delete must expand")
        };
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].variant_name(), "Split");
    }

    #[test]
    fn lift_of_a_whole_clip_needs_no_splits() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a"), (100, 50, "b")])]);
        let v1 = track_id(&tl, "V1");
        let effect = roundtrip(
            &mut tl,
            &EditCommand::Lift {
                track: v1,
                start: Frame(0),
                end: Frame(100),
            },
        );
        assert_eq!(tl.dump(), "V1:<gap 100>[b 100-150]\nA1: -\n");
        assert_eq!(effect.applied.variant_name(), "Lift");
    }

    #[test]
    fn insert_overwrite_and_paste_round_trip() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        roundtrip(
            &mut tl,
            &EditCommand::Insert {
                track: v1,
                at: Frame(40),
                clip: sample_clip(),
                new_id: None,
            },
        );
        assert_eq!(tl.dump(), "V1:[a 0-40][n 40-70][a 70-130]\nA1: -\n");

        roundtrip(
            &mut tl,
            &EditCommand::Overwrite {
                track: v1,
                at: Frame(10),
                clip: sample_clip(),
                new_id: None,
            },
        );
        assert_eq!(
            tl.dump(),
            "V1:[a 0-10][n 10-40][n 40-70][a 70-130]\nA1: -\n"
        );

        let register = tl.yank_range(v1, Frame(0), Frame(40)).unwrap();
        roundtrip(
            &mut tl,
            &EditCommand::Paste {
                track: v1,
                at: Frame(0),
                register,
                ripple: true,
            },
        );
    }

    #[test]
    fn a_pasted_clip_is_new_material_with_no_linkage() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "a-aud")])]);
        let v1 = track_id(&tl, "V1");
        let clips = [clip_ids(&tl, "V1")[0], clip_ids(&tl, "A1")[0]];
        EditCommand::Link {
            clips: clips.to_vec(),
            group: None,
        }
        .apply(&mut tl)
        .unwrap();
        let register = tl.yank_range(v1, Frame(0), Frame(100)).unwrap();
        assert!(register.clips[0].group.is_some());

        EditCommand::Paste {
            track: v1,
            at: Frame(100),
            register,
            ripple: true,
        }
        .apply(&mut tl)
        .unwrap();
        let pasted = tl.track(v1).unwrap().clips()[1].clone();
        assert!(pasted.group.is_none());
        tl.assert_invariants();
    }

    #[test]
    fn move_clip_round_trips_and_keeps_linkage() {
        let mut tl = fixture(&[("V1", &[(0, 50, "a"), (100, 50, "b")])]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        roundtrip(
            &mut tl,
            &EditCommand::MoveClip {
                track: v1,
                clip: a,
                to: Frame(200),
            },
        );
        assert_eq!(
            tl.dump(),
            "V1:<gap 100>[b 100-150]<gap 50>[a 200-250]\nA1: -\n"
        );
    }

    #[test]
    fn the_trim_family_round_trips() {
        let mut tl = media_fixture(&[(0, 100, 50, 300), (100, 100, 50, 300), (200, 100, 50, 300)]);
        let v1 = track_id(&tl, "V1");
        let ids = clip_ids(&tl, "V1");
        roundtrip(
            &mut tl,
            &EditCommand::Trim {
                track: v1,
                clip: ids[0],
                edge: Edge::Tail,
                delta: 20,
            },
        );
        roundtrip(
            &mut tl,
            &EditCommand::Slip {
                track: v1,
                clip: ids[1],
                delta: -10,
            },
        );
        roundtrip(
            &mut tl,
            &EditCommand::Slide {
                track: v1,
                clip: ids[1],
                delta: 15,
            },
        );
        let cut = tl.find_clip(ids[1]).unwrap().1.start;
        roundtrip(
            &mut tl,
            &EditCommand::Roll {
                track: v1,
                cut,
                delta: -5,
            },
        );
    }

    #[test]
    fn grouping_and_properties_round_trip() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "a-aud")])]);
        let v1 = track_id(&tl, "V1");
        let clips = vec![clip_ids(&tl, "V1")[0], clip_ids(&tl, "A1")[0]];
        roundtrip(
            &mut tl,
            &EditCommand::Link {
                clips: clips.clone(),
                group: None,
            },
        );
        roundtrip(
            &mut tl,
            &EditCommand::SetGroup {
                clip: clips[0],
                group: None,
            },
        );
        roundtrip(
            &mut tl,
            &EditCommand::SetProps {
                track: v1,
                clip: clips[0],
                props: ClipProps {
                    gain_db: -6.0,
                    fade_in: Frame(10),
                    ..ClipProps::default()
                },
            },
        );
    }

    #[test]
    fn a_rejected_command_changes_nothing_at_all() {
        let mut tl = media_fixture(&[(0, 100, 0, 120)]);
        let v1 = track_id(&tl, "V1");
        let a = clip_ids(&tl, "V1")[0];
        let before = json(&tl);

        // Every one of these fails validation - including the id generator,
        // which is part of the serialized state.
        let rejected = [
            EditCommand::Split {
                track: v1,
                frame: Frame(0),
                new_id: None,
            },
            EditCommand::Trim {
                track: v1,
                clip: a,
                edge: Edge::Tail,
                delta: 500,
            },
            EditCommand::Insert {
                track: v1,
                at: Frame(0),
                clip: Clip::generated(ClipId(0), "z", Frame::ZERO, Frame::ZERO),
                new_id: None,
            },
            EditCommand::Paste {
                track: v1,
                at: Frame(0),
                register: Register::default(),
                ripple: true,
            },
            EditCommand::Link {
                clips: vec![a],
                group: None,
            },
            EditCommand::Join {
                track: v1,
                frame: Frame(50),
            },
        ];
        for cmd in &rejected {
            assert!(
                cmd.apply(&mut tl).is_err(),
                "{} was accepted",
                cmd.describe()
            );
            assert_eq!(json(&tl), before, "{} mutated the timeline", cmd.describe());
        }
    }

    #[test]
    fn a_sequence_rolls_back_completely_when_a_step_fails() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let before = json(&tl);
        let cmd = EditCommand::Sequence(vec![
            EditCommand::Split {
                track: v1,
                frame: Frame(40),
                new_id: None,
            },
            // No clip at 500: this step is rejected, so the split unwinds.
            EditCommand::Split {
                track: v1,
                frame: Frame(500),
                new_id: None,
            },
        ]);
        assert!(cmd.apply(&mut tl).is_err());
        assert_eq!(json(&tl), before);
    }

    #[test]
    fn repeat_drops_pinned_ids_so_it_can_run_again() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let cmd = EditCommand::Split {
            track: v1,
            frame: Frame(40),
            new_id: None,
        };
        let effect = cmd.apply(&mut tl).unwrap();
        // Replaying the pinned form would collide; the repeat form mints.
        assert!(effect.applied.apply(&mut tl).is_err());
        let repeat = effect.applied.for_repeat();
        assert!(
            matches!(repeat, EditCommand::Split { new_id: None, .. }),
            "repeat must not pin an id"
        );
        assert!(
            EditCommand::Split {
                track: v1,
                frame: Frame(70),
                new_id: None
            }
            .apply(&mut tl)
            .is_ok()
        );
    }

    #[test]
    fn a_materialised_restore_repeats_as_a_paste() {
        let restore = EditCommand::Restore {
            track: TrackId(1),
            at: Frame(0),
            clips: vec![sample_clip()],
            span: Frame(30),
            ripple: true,
        };
        assert_eq!(restore.for_repeat().variant_name(), "Paste");
    }

    /// Every variant must have a sample here. The exhaustive match in
    /// `variant_name` means a new variant will not compile until it is named,
    /// and this assertion means it will not pass until it is covered.
    fn one_of_each() -> Vec<EditCommand> {
        let track = TrackId(1);
        let clip = ClipId(3);
        vec![
            EditCommand::Split {
                track,
                frame: Frame(10),
                new_id: Some(ClipId(9)),
            },
            EditCommand::Join {
                track,
                frame: Frame(10),
            },
            EditCommand::Lift {
                track,
                start: Frame(0),
                end: Frame(5),
            },
            EditCommand::RippleDelete {
                track,
                start: Frame(0),
                end: Frame(5),
            },
            EditCommand::Insert {
                track,
                at: Frame(0),
                clip: sample_clip(),
                new_id: Some(ClipId(9)),
            },
            EditCommand::Overwrite {
                track,
                at: Frame(0),
                clip: sample_clip(),
                new_id: None,
            },
            EditCommand::Paste {
                track,
                at: Frame(0),
                register: Register {
                    clips: vec![sample_clip()],
                    span: Frame(30),
                },
                ripple: true,
            },
            EditCommand::Restore {
                track,
                at: Frame(0),
                clips: vec![sample_clip()],
                span: Frame(30),
                ripple: false,
            },
            EditCommand::MoveClip {
                track,
                clip,
                to: Frame(99),
            },
            EditCommand::Trim {
                track,
                clip,
                edge: Edge::Head,
                delta: -4,
            },
            EditCommand::Roll {
                track,
                cut: Frame(10),
                delta: 4,
            },
            EditCommand::Slip {
                track,
                clip,
                delta: 4,
            },
            EditCommand::Slide {
                track,
                clip,
                delta: 4,
            },
            EditCommand::Link {
                clips: vec![clip, ClipId(4)],
                group: Some(GroupId(2)),
            },
            EditCommand::SetGroup {
                clip,
                group: Some(GroupId(2)),
            },
            EditCommand::AddTrack {
                kind: TrackKind::Audio,
                name: Some("A9".into()),
                new_id: Some(TrackId(42)),
            },
            EditCommand::RemoveTrack { track },
            EditCommand::Reconform {
                props: TimelineProps::default(),
            },
            EditCommand::RestoreConform {
                state: Box::new(fixture(&[("V1", &[(0, 10, "a")])]).conform_state()),
            },
            EditCommand::SetProps {
                track,
                clip,
                props: ClipProps::default(),
            },
            EditCommand::Sequence(vec![EditCommand::Join {
                track,
                frame: Frame(1),
            }]),
        ]
    }

    #[test]
    fn every_variant_serializes_round_trip() {
        let mut seen: Vec<&str> = Vec::new();
        for cmd in one_of_each() {
            let text = serde_json::to_string(&cmd).unwrap();
            let back: EditCommand = serde_json::from_str(&text).unwrap();
            assert_eq!(back, cmd, "{text} did not survive a round trip");
            assert!(!cmd.describe().is_empty());
            seen.push(cmd.variant_name());
        }
        for name in VARIANT_NAMES {
            assert!(seen.contains(name), "no sample command for {name}");
        }
        assert_eq!(seen.len(), VARIANT_NAMES.len());
    }

    #[test]
    fn adding_a_track_is_undoable_and_redoes_to_the_same_id() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let effect = roundtrip(
            &mut tl,
            &EditCommand::AddTrack {
                kind: TrackKind::Audio,
                name: None,
                new_id: None,
            },
        );
        match effect.applied {
            // The log must pin both the id and the generated name, or a redo
            // could name the track differently.
            EditCommand::AddTrack { name, new_id, .. } => {
                assert_eq!(name.as_deref(), Some("A2"));
                assert!(new_id.is_some());
            }
            other => panic!("unexpected applied form: {other:?}"),
        }
    }

    #[test]
    fn a_track_with_clips_on_it_cannot_be_removed() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")])]);
        let v1 = track_id(&tl, "V1");
        let before = json(&tl);
        assert!(
            EditCommand::RemoveTrack { track: v1 }
                .apply(&mut tl)
                .is_err()
        );
        assert_eq!(json(&tl), before);
    }

    #[test]
    fn reconform_is_one_undoable_command_that_restores_exactly() {
        // plan.md Phase 5: "change timeline.fps with clips present, assert it
        // is a single undoable command that restores exactly on undo".
        let mut tl = fixture(&[
            ("V1", &[(0, 100, "a"), (100, 3, "b")]),
            ("A1", &[(0, 250, "c")]),
        ]);
        tl.props = TimelineProps {
            fps: davimci_core::Fps::FPS_30,
            ..TimelineProps::default()
        };
        let effect = roundtrip(
            &mut tl,
            &EditCommand::Reconform {
                props: TimelineProps {
                    fps: davimci_core::Fps::FPS_23_976,
                    ..TimelineProps::default()
                },
            },
        );
        assert!(matches!(effect.inverse, EditCommand::RestoreConform { .. }));
        assert_eq!(tl.props.fps, davimci_core::Fps::FPS_23_976);
    }

    #[test]
    fn commands_reject_a_clip_on_another_track() {
        let mut tl = fixture(&[("V1", &[(0, 100, "a")]), ("A1", &[(0, 100, "b")])]);
        let a1 = track_id(&tl, "A1");
        let v_clip = clip_ids(&tl, "V1")[0];
        assert!(
            EditCommand::MoveClip {
                track: a1,
                clip: v_clip,
                to: Frame(200)
            }
            .apply(&mut tl)
            .is_err()
        );
    }

    #[test]
    fn media_clips_survive_a_delete_and_restore_unchanged() {
        let mut tl = Timeline::new(TimelineProps::default());
        let v1 = tl.tracks().first().map(|t| t.id).unwrap_or(TrackId(0));
        let id = tl.new_clip_id();
        let media = MediaRef::new("/m.mkv", davimci_core::Fps::FPS_60, Frame(600));
        let clip = Clip::from_media(id, "m", media, Frame::ZERO, Frame(30), Frame(120));
        EditCommand::Insert {
            track: v1,
            at: Frame::ZERO,
            clip,
            new_id: Some(id),
        }
        .apply(&mut tl)
        .unwrap();
        roundtrip(
            &mut tl,
            &EditCommand::RippleDelete {
                track: v1,
                start: Frame(20),
                end: Frame(60),
            },
        );
    }
}
