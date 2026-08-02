//! Where the grammar becomes meaning: [`Engine`] turns a parsed [`Action`]
//! into motion resolution (`davimci-motion`) and command execution
//! (`davimci-cmd`), against a live [`Session`].
//!
//! Transport (`<Space><Space>`, `J`/`K`/`L`, ...) is deliberately *not*
//! dispatched through [`Session::exec`]: spec §3.2.1 is explicit that
//! playback is not an edit. [`Engine::feed`] returns a [`TransportCmd`] for
//! the caller to hand to the render backend's clock (plan.md Phase 6).

use std::collections::HashMap;

use davimci_cmd::{EditCommand, Session};
use davimci_core::{Frame, Register, Timeline, TrackId};
use davimci_motion::{
    BuiltinMotion, JumpConfig, Motion as MotionTrait, MotionCtx, Object as ObjectTrait, Resolved,
    Scope, TextObject, TimeRange, Zoom,
};

use crate::action::{Action, Operator, Target};
use crate::error::KeysError;
use crate::key::Key;
use crate::keymap::Keymap;
use crate::mode::{Anchor, Mode, ModeChanged, ModeState};
use crate::parser::{Parser, Step};

/// A transport action, handed to the backend clock rather than the undo log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportCmd {
    PlayPause,
    ShuttleForward,
    ShuttleBackward,
    ShuttleStop,
    PreviewAndReturn,
    LoopSelection,
}

/// What happened after feeding one key through the [`Engine`].
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A sequence is still being typed.
    Pending,
    /// `Esc` cancelled a pending sequence; the mode did not necessarily
    /// change (there was nothing pending to cancel out of).
    Cancelled,
    /// The key sequence typed cannot resolve to anything.
    Invalid,
    /// A command applied; carries its one-line description.
    Applied(String),
    /// The playhead or track focus moved.
    Moved,
    /// A predicate motion's analysis has not finished.
    PredicatePending,
    /// The mode changed (`ModeChanged` event, spec §9's `autocmd` hook).
    Mode(ModeChanged),
    MacroStarted(char),
    MacroStopped(char),
    /// `@a`: the outcomes of replaying the macro's tokens, in order.
    Replayed(Vec<Outcome>),
    /// A transport action; not run through the undo log.
    Transport(TransportCmd),
    /// `:` was pressed; the caller now owns command-line input.
    EnterCommandMode,
    /// A Lua-bound key fired; the host must run callback `.0` through
    /// `davimci_lua::Runtime::invoke` and execute the requests it returns.
    Plugin(u32),
    /// Something named in spec but not yet backed by a command (e.g.
    /// transitions, Phase 9f).
    NotImplemented(&'static str),
    /// Rejected: the message is user-facing text, never `Debug` output.
    Error(String),
}

/// Ties the key grammar to a live session. One `Engine` per open timeline.
#[derive(Debug)]
pub struct Engine {
    keymap: Keymap,
    parser: Parser,
    mode: ModeState,
    jump_cfg: JumpConfig,
    zoom: Zoom,
    /// Registers named with `"<reg>` (spec §11's `"ayy`); distinct from the
    /// clipboard/anonymous register used when none is named.
    registers: HashMap<char, Register>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            keymap: Keymap::new(),
            parser: Parser::new(),
            mode: ModeState::new(),
            jump_cfg: JumpConfig::default(),
            zoom: Zoom::default(),
            registers: HashMap::new(),
        }
    }

    #[must_use]
    pub fn with_keymap(keymap: Keymap) -> Self {
        Self {
            keymap,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode.mode()
    }

    #[must_use]
    pub fn mode_state(&self) -> &ModeState {
        &self.mode
    }

    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
    }

    /// Feed one key through the grammar and, once a sequence completes, run
    /// it against `session`.
    pub fn feed(&mut self, key: Key, session: &mut Session) -> Outcome {
        // Vim always lets a bare `q` stop an active recording, regardless of
        // what the grammar would otherwise make of it.
        if key == Key::Char('q')
            && !self.parser.is_pending()
            && session.macros().recording().is_some()
        {
            return match session.macros_mut().stop() {
                Ok(r) => Outcome::MacroStopped(r),
                Err(e) => Outcome::Error(e.to_string()),
            };
        }
        if session.macros().recording().is_some() {
            session.macros_mut().push(key.to_token());
        }
        match self.parser.feed(key, &self.keymap, self.mode.mode()) {
            Step::Pending => Outcome::Pending,
            Step::Cancelled => Outcome::Mode(self.mode.escape()),
            Step::Invalid => Outcome::Invalid,
            Step::Complete(action) => self.execute(action, session),
        }
    }

    /// Owned rather than cached: [`JumpPointCache`] ties its return value's
    /// lifetime to `&mut self`, which cannot coexist with the `&Timeline`
    /// borrows every caller here also needs. Caching is worth restoring once
    /// a frontend actually calls this on a hot path (plan.md Phase 9a).
    fn jump_points(&self, tl: &Timeline) -> davimci_motion::JumpPoints {
        davimci_motion::JumpPoints::build(
            tl,
            Some(tl.playhead().track),
            self.zoom,
            &self.jump_cfg,
            &[],
        )
    }

    /// Run an already-parsed action. A frontend needs this for actions that
    /// did not come from a keystroke - a Lua plugin callback asking for an
    /// edit (spec §9.2) goes through here, so plugin edits are ordinary
    /// commands with ordinary undo.
    pub fn execute_action(&mut self, action: Action, session: &mut Session) -> Outcome {
        self.execute(action, session)
    }

    fn execute(&mut self, action: Action, session: &mut Session) -> Outcome {
        match action {
            Action::Move { motion, count } => self.do_move(motion, count, session),
            Action::Verb {
                op,
                count,
                register,
                target,
            } => match self.do_verb(op, count, register, target, session) {
                Ok(o) => o,
                Err(e) => Outcome::Error(e.user_message_pub()),
            },
            Action::SplitCurrent => self.do_split(false, session),
            Action::SplitAll => self.do_split(true, session),
            Action::RippleDeleteClip => self.do_ripple_delete_clip(session),
            Action::Paste {
                before,
                ripple,
                register,
            } => self.do_paste(before, ripple, register, session),
            Action::Replace | Action::InsertMedia | Action::AppendMedia => {
                Outcome::NotImplemented("media import is Phase 5")
            }
            Action::Undo => run(session.undo()),
            Action::Redo => run(session.redo()),
            Action::Repeat => run(session.repeat()),
            Action::MacroStart(reg) => match session.macros_mut().start(reg) {
                Ok(()) => Outcome::MacroStarted(reg),
                Err(e) => Outcome::Error(e.to_string()),
            },
            Action::MacroStop => match session.macros_mut().stop() {
                Ok(r) => Outcome::MacroStopped(r),
                Err(e) => Outcome::Error(e.to_string()),
            },
            Action::MacroReplay(reg, count) => self.do_replay(reg, count, session),
            Action::SetMark(name) => {
                let p = session.timeline().playhead();
                session.set_mark(name, p.frame, Some(p.track));
                Outcome::Applied(format!("mark '{name}' set at {}", p.frame))
            }
            Action::JumpMark(name) => self.do_jump_mark(name, session),
            Action::EnterVisual(kind) => {
                let p = session.timeline().playhead();
                Outcome::Mode(self.mode.toggle_visual(
                    kind,
                    Anchor {
                        frame: p.frame,
                        track: p.track,
                    },
                ))
            }
            Action::SwapVisualEnds => {
                self.mode.swap_visual_ends();
                Outcome::Moved
            }
            Action::ToggleVisualTrack => {
                let t = session.timeline().playhead().track;
                self.mode.toggle_visual_track(t);
                Outcome::Moved
            }
            Action::TrimEdgeStep { .. } => Outcome::NotImplemented("jump-point edge trim"),
            Action::GainAdjust(step) => self.do_gain(step, session),
            Action::CreateTransition | Action::DeleteTransition => {
                Outcome::NotImplemented("transitions land in Phase 9f")
            }
            Action::PlayPause => Outcome::Transport(TransportCmd::PlayPause),
            Action::Shuttle { forward } => Outcome::Transport(if forward {
                TransportCmd::ShuttleForward
            } else {
                TransportCmd::ShuttleBackward
            }),
            Action::ShuttleStop => Outcome::Transport(TransportCmd::ShuttleStop),
            Action::PreviewAndReturn => Outcome::Transport(TransportCmd::PreviewAndReturn),
            Action::LoopSelection => Outcome::Transport(TransportCmd::LoopSelection),
            Action::Plugin(id) => Outcome::Plugin(id),
            Action::EnterCommandMode => {
                let c = self.mode.enter(Mode::Command);
                Outcome::Mode(c)
            }
            Action::Escape => Outcome::Mode(self.mode.escape()),
        }
    }

    fn do_move(&mut self, motion: BuiltinMotion, count: u32, session: &mut Session) -> Outcome {
        let resolved = {
            let tl = session.timeline();
            let jumps = self.jump_points(tl);
            let ctx = MotionCtx::new(tl, &jumps);
            motion.resolve(&ctx, count)
        };
        match resolved {
            Ok(Resolved::Position(p)) => {
                if self.mode.mode().is_visual() {
                    self.mode.extend_visual(
                        Anchor {
                            frame: p.frame,
                            track: p.track,
                        },
                        davimci_motion::Direction::Forward,
                    );
                } else if let Err(e) = session.set_playhead(p.frame, p.track) {
                    return Outcome::Error(e.to_string());
                }
                Outcome::Moved
            }
            Ok(Resolved::Range(..)) => Outcome::Invalid,
            Ok(Resolved::Pending) => Outcome::PredicatePending,
            Err(e) => Outcome::Error(e.to_string()),
        }
    }

    fn target_range(
        &mut self,
        target: &Target,
        session: &Session,
    ) -> Result<Option<(TimeRange, Scope)>, KeysError> {
        let tl = session.timeline();
        let playhead = tl.playhead();
        match target {
            Target::WholeClip => {
                let jumps = self.jump_points(tl);
                let ctx = MotionCtx::new(tl, &jumps);
                match TextObject::InnerClip.resolve(&ctx)? {
                    Resolved::Range(r, s) => Ok(Some((r, s))),
                    _ => Ok(None),
                }
            }
            Target::Object(obj) => {
                let jumps = self.jump_points(tl);
                let ctx = MotionCtx::new(tl, &jumps);
                match obj.resolve(&ctx)? {
                    Resolved::Range(r, s) => Ok(Some((r, s))),
                    Resolved::Pending => Ok(None),
                    Resolved::Position(_) => Ok(None),
                }
            }
            Target::Motion(m, count) => {
                let jumps = self.jump_points(tl);
                let ctx = MotionCtx::new(tl, &jumps);
                match m.resolve(&ctx, *count)? {
                    Resolved::Position(p) => Ok(Some((
                        TimeRange::new(playhead.frame, p.frame),
                        Scope::single(playhead.track),
                    ))),
                    Resolved::Pending => Ok(None),
                    Resolved::Range(r, s) => Ok(Some((r, s))),
                }
            }
            Target::Visual => Ok(self
                .mode
                .visual()
                .map(|v| (v.range(), Scope::new(v.tracks.iter().copied())))),
        }
    }

    fn do_verb(
        &mut self,
        op: Operator,
        _count: u32,
        register: Option<char>,
        target: Target,
        session: &mut Session,
    ) -> Result<Outcome, KeysError> {
        match op {
            Operator::Yank => return self.do_yank(&target, register, session),
            Operator::RippleTrim | Operator::Roll | Operator::Slip | Operator::Slide => {
                return self.do_edge_op(op, &target, session);
            }
            Operator::Fade => return self.do_fade(&target, session),
            _ => {}
        }
        let Some((range, scope)) = self.target_range(&target, session)? else {
            return Ok(Outcome::PredicatePending);
        };
        if scope.is_empty() || range.is_empty() {
            return Err(KeysError::EmptyTarget);
        }
        let cmds: Vec<EditCommand> = scope
            .tracks()
            .iter()
            .map(|&track| match op {
                Operator::RippleDelete | Operator::Change => Ok(EditCommand::RippleDelete {
                    track,
                    start: range.start,
                    end: range.end,
                }),
                Operator::Lift => Ok(EditCommand::Lift {
                    track,
                    start: range.start,
                    end: range.end,
                }),
                // Every other operator returned above. A library crate
                // reports this rather than panicking.
                _ => Err(KeysError::Internal("that operator has no range form")),
            })
            .collect::<Result<_, _>>()?;
        let cmd = match <[EditCommand; 1]>::try_from(cmds) {
            Ok([one]) => one,
            Err(many) => EditCommand::Sequence(many),
        };
        let label = session.exec(&cmd)?;
        if op == Operator::Change {
            self.mode.enter(Mode::Insert);
        } else if matches!(target, Target::Visual) {
            // A verb applied to a selection ends the selection, as in vim.
            self.mode.escape();
        }
        Ok(Outcome::Applied(label))
    }

    fn do_yank(
        &mut self,
        target: &Target,
        register: Option<char>,
        session: &Session,
    ) -> Result<Outcome, KeysError> {
        let Some((range, scope)) = self.target_range(target, session)? else {
            return Ok(Outcome::PredicatePending);
        };
        let Some(&track) = scope.tracks().first() else {
            return Err(KeysError::EmptyTarget);
        };
        let reg = session
            .timeline()
            .yank_range(track, range.start, range.end)
            .map_err(davimci_cmd::CmdError::from)?;
        self.registers.insert(register.unwrap_or('"'), reg);
        Ok(Outcome::Applied(format!(
            "yanked {}-{}",
            range.start, range.end
        )))
    }

    fn do_paste(
        &mut self,
        before: bool,
        ripple: bool,
        register: Option<char>,
        session: &mut Session,
    ) -> Outcome {
        let Some(reg) = self.registers.get(&register.unwrap_or('"')).cloned() else {
            return Outcome::Error("register is empty".to_string());
        };
        let p = session.timeline().playhead();
        let at = if before {
            p.frame
        } else {
            Frame(p.frame.get().saturating_add(1))
        };
        run(session.exec(&EditCommand::Paste {
            track: p.track,
            at,
            register: reg,
            ripple,
        }))
    }

    fn do_split(&mut self, all_tracks: bool, session: &mut Session) -> Outcome {
        let p = session.timeline().playhead();
        let tracks: Vec<TrackId> = if all_tracks {
            session
                .timeline()
                .tracks()
                .iter()
                .filter(|t| interior(t.clips().iter().map(|c| (c.start, c.end())), p.frame))
                .map(|t| t.id)
                .collect()
        } else {
            vec![p.track]
        };
        if tracks.is_empty() {
            return Outcome::Error("no clip under the playhead to split".to_string());
        }
        let cmds: Vec<EditCommand> = tracks
            .into_iter()
            .map(|track| EditCommand::Split {
                track,
                frame: p.frame,
                new_id: None,
            })
            .collect();
        let cmd = if cmds.len() == 1 {
            cmds.into_iter()
                .next()
                .unwrap_or(EditCommand::Sequence(vec![]))
        } else {
            EditCommand::Sequence(cmds)
        };
        run(session.exec(&cmd))
    }

    fn do_ripple_delete_clip(&mut self, session: &mut Session) -> Outcome {
        let target = Target::WholeClip;
        match self.target_range(&target, session) {
            Ok(Some((range, scope))) => {
                let Some(&track) = scope.tracks().first() else {
                    return Outcome::Error("no clip under the playhead".to_string());
                };
                run(session.exec(&EditCommand::RippleDelete {
                    track,
                    start: range.start,
                    end: range.end,
                }))
            }
            Ok(None) => Outcome::PredicatePending,
            Err(e) => Outcome::Error(e.user_message_pub()),
        }
    }

    fn do_edge_op(
        &mut self,
        op: Operator,
        target: &Target,
        session: &mut Session,
    ) -> Result<Outcome, KeysError> {
        let Target::Motion(m, count) = target else {
            return Err(KeysError::EmptyTarget);
        };
        let (playhead, target_frame) = {
            let tl = session.timeline();
            let p = tl.playhead();
            let jumps = self.jump_points(tl);
            let ctx = MotionCtx::new(tl, &jumps);
            match m.resolve(&ctx, *count)? {
                Resolved::Position(pos) => (p, pos.frame),
                _ => return Ok(Outcome::PredicatePending),
            }
        };
        let track = playhead.track;
        let cmd = match op {
            Operator::Slip => {
                let clip = clip_under(session.timeline(), track, playhead.frame)
                    .ok_or(KeysError::EmptyTarget)?;
                EditCommand::Slip {
                    track,
                    clip,
                    delta: delta_of(playhead.frame, target_frame),
                }
            }
            Operator::Slide => {
                let clip = clip_under(session.timeline(), track, playhead.frame)
                    .ok_or(KeysError::EmptyTarget)?;
                EditCommand::Slide {
                    track,
                    clip,
                    delta: delta_of(playhead.frame, target_frame),
                }
            }
            Operator::RippleTrim => {
                let (clip, edge) = nearest_edge(session.timeline(), track, playhead.frame)
                    .ok_or(KeysError::EmptyTarget)?;
                let anchor = edge_frame(session.timeline(), track, clip, edge)
                    .ok_or(KeysError::EmptyTarget)?;
                EditCommand::Trim {
                    track,
                    clip,
                    edge,
                    delta: delta_of(anchor, target_frame),
                }
            }
            Operator::Roll => {
                let cut = nearest_cut(session.timeline(), track, playhead.frame)
                    .ok_or(KeysError::EmptyTarget)?;
                EditCommand::Roll {
                    track,
                    cut,
                    delta: delta_of(cut, target_frame),
                }
            }
            // Only the four edge operators are dispatched here.
            _ => return Err(KeysError::Internal("that operator is not an edge trim")),
        };
        Ok(run(session.exec(&cmd)))
    }

    fn do_fade(&mut self, target: &Target, session: &mut Session) -> Result<Outcome, KeysError> {
        let Target::Motion(m, count) = target else {
            return Err(KeysError::EmptyTarget);
        };
        let (track, playhead_frame, target_frame) = {
            let tl = session.timeline();
            let p = tl.playhead();
            let jumps = self.jump_points(tl);
            let ctx = MotionCtx::new(tl, &jumps);
            match m.resolve(&ctx, *count)? {
                Resolved::Position(pos) => (p.track, p.frame, pos.frame),
                _ => return Ok(Outcome::PredicatePending),
            }
        };
        let clip =
            clip_under(session.timeline(), track, playhead_frame).ok_or(KeysError::EmptyTarget)?;
        let (_, c) = session
            .timeline()
            .find_clip(clip)
            .ok_or(KeysError::EmptyTarget)?;
        let mut props = c.props;
        let span = Frame(playhead_frame.get().abs_diff(target_frame.get()));
        if target_frame >= playhead_frame {
            props.fade_in = span;
        } else {
            props.fade_out = span;
        }
        Ok(run(session.exec(&EditCommand::SetProps {
            track,
            clip,
            props,
        })))
    }

    fn do_gain(&mut self, step_db: i32, session: &mut Session) -> Outcome {
        let p = session.timeline().playhead();
        let Some(clip) = clip_under(session.timeline(), p.track, p.frame) else {
            return Outcome::Error("no clip under the playhead".to_string());
        };
        let Some((_, c)) = session.timeline().find_clip(clip) else {
            return Outcome::Error("no such clip".to_string());
        };
        let mut props = c.props;
        props.gain_db += step_db as f32;
        run(session.exec(&EditCommand::SetProps {
            track: p.track,
            clip,
            props,
        }))
    }

    fn do_jump_mark(&mut self, name: char, session: &mut Session) -> Outcome {
        let Some(mark) = session.timeline().marks.get(&name).copied() else {
            return Outcome::Error(format!("mark '{name}' is not set"));
        };
        let track = mark
            .track
            .unwrap_or_else(|| session.timeline().playhead().track);
        match session.set_playhead(mark.frame, track) {
            Ok(()) => Outcome::Moved,
            Err(e) => Outcome::Error(e.to_string()),
        }
    }

    fn do_replay(&mut self, reg: char, count: u32, session: &mut Session) -> Outcome {
        let tokens = match session.macros().replay(reg) {
            Ok(t) => t.to_vec(),
            Err(e) => return Outcome::Error(e.to_string()),
        };
        let mut out = Vec::new();
        for _ in 0..count.max(1) {
            for tok in &tokens {
                for key in Key::parse_str(tok) {
                    out.push(self.feed(key, session));
                }
            }
        }
        Outcome::Replayed(out)
    }
}

fn run(result: Result<String, davimci_cmd::CmdError>) -> Outcome {
    match result {
        Ok(label) => Outcome::Applied(label),
        Err(e) => Outcome::Error(e.to_string()),
    }
}

fn interior(bounds: impl Iterator<Item = (Frame, Frame)>, frame: Frame) -> bool {
    bounds.into_iter().any(|(s, e)| frame > s && frame < e)
}

fn clip_under(tl: &Timeline, track: TrackId, frame: Frame) -> Option<davimci_core::ClipId> {
    tl.track(track)?.clip_at(frame).map(|c| c.id)
}

fn delta_of(from: Frame, to: Frame) -> i64 {
    i64::try_from(to.get()).unwrap_or(i64::MAX) - i64::try_from(from.get()).unwrap_or(i64::MAX)
}

/// The edge of the clip under `frame` nearest to `frame`.
fn nearest_edge(
    tl: &Timeline,
    track: TrackId,
    frame: Frame,
) -> Option<(davimci_core::ClipId, davimci_core::Edge)> {
    let t = tl.track(track)?;
    let c = t.clip_at(frame).or_else(|| t.clips().last())?;
    let to_head = frame.get().abs_diff(c.start.get());
    let to_tail = frame.get().abs_diff(c.end().get());
    Some((
        c.id,
        if to_head <= to_tail {
            davimci_core::Edge::Head
        } else {
            davimci_core::Edge::Tail
        },
    ))
}

fn edge_frame(
    tl: &Timeline,
    track: TrackId,
    clip: davimci_core::ClipId,
    edge: davimci_core::Edge,
) -> Option<Frame> {
    let t = tl.track(track)?;
    let c = t.clip(clip)?;
    Some(match edge {
        davimci_core::Edge::Head => c.start,
        davimci_core::Edge::Tail => c.end(),
    })
}

/// The clip boundary nearest `frame` on `track`.
fn nearest_cut(tl: &Timeline, track: TrackId, frame: Frame) -> Option<Frame> {
    let t = tl.track(track)?;
    t.clips()
        .iter()
        .flat_map(|c| [c.start, c.end()])
        .min_by_key(|f| f.get().abs_diff(frame.get()))
}

// `thiserror`'s generated `Display` already gives a full sentence; this just
// spells the intent at call sites above without importing `Classify` there.
impl KeysError {
    fn user_message_pub(&self) -> String {
        self.to_string()
    }
}
