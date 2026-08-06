//! Where the grammar becomes meaning: [`Engine`] turns a parsed [`Action`]
//! into motion resolution (`davimci-motion`) and command execution
//! (`davimci-cmd`), against a live [`Session`].
//!
//! Transport (`<Space><Space>`, `J`/`K`/`L`, ...) is deliberately *not*
//! dispatched through [`Session::exec`], because playback is not an edit. [`Engine::feed`] returns a [`TransportCmd`] for
//! the caller to hand to the render backend's clock.

use std::collections::HashMap;

use davimci_cmd::{EditCommand, Session};
use davimci_core::{
    ClipId, ClipProps, DEFAULT_TRANSITION, DEFAULT_TRANSITION_FRAMES, Frame, Register, Selection,
    Timeline, TrackId, Transition,
};
use davimci_motion::{
    BuiltinMotion, JumpConfig, Motion as MotionTrait, MotionCtx, Object as ObjectTrait, Resolved,
    Scope, TextObject, TimeRange, Zoom,
};

use crate::action::{Action, Operator, Target, TransportPolicy, ZoomIntent};
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
    /// Stop playback and commit the playhead. Unlike
    /// [`TransportCmd::PlayPause`] this never toggles: interrupting a stopped
    /// transport does nothing.
    Interrupt,
}

/// What a chosen media file is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaIntent {
    /// `i`: insert at the playhead, rippling later clips right.
    Insert,
    /// `a`: append after the clip under the playhead.
    Append,
    /// `r`: replace the clip under the playhead.
    Replace,
}

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
    /// The mode changed (`ModeChanged` event, for Lua `autocmd`s).
    Mode(ModeChanged),
    MacroStarted(char),
    MacroStopped(char),
    /// `@a`: the outcomes of replaying the macro's tokens, in order.
    Replayed(Vec<Outcome>),
    /// A transport action; not run through the undo log.
    Transport(TransportCmd),
    /// `zi`/`zo`/`z0`: the host owns the viewport, so zoom is reported
    /// rather than applied. Not an edit and never undoable.
    Zoom(ZoomIntent),
    /// `:` was pressed; the caller now owns command-line input.
    EnterCommandMode,
    /// `i`/`a`/`r`: the caller should open the media picker. The grammar has
    /// no filesystem and no idea what media exists, so choosing the file is
    /// the host's job; this only says what the chosen file is *for*.
    PickMedia(MediaIntent),
    /// `i` on a subtitle clip: INSERT mode is scoped to text editing there
    ///, so the caller should open a text buffer rather than
    /// a media picker.
    EditText {
        clip: ClipId,
        text: String,
    },
    /// A Lua-bound key fired; the host must run callback `.0` through
    /// `davimci_lua::Runtime::invoke` and execute the requests it returns.
    Plugin(u32),
    /// A config-registered text object was typed. Only the host
    /// has the Lua runtime, so it resolves the name and re-issues the verb
    /// with [`Engine::execute_action`] and the range it answered.
    ResolveObject {
        name: char,
        around: bool,
        /// The verb to run once the range is known.
        verb: Box<Action>,
    },
    /// A keybinding parsed but not yet backed by a command.
    NotImplemented(&'static str),
    /// Rejected: the message is user-facing text, never `Debug` output.
    Error(String),
}

/// One key's worth of result: what happened, and what it means for a running
/// preview.
///
/// The policy travels with the outcome rather than being looked up later
/// because the host never sees the [`Action`]: only the engine knows which
/// binding the keys resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    pub outcome: Outcome,
    pub transport: TransportPolicy,
}

impl Feed {
    fn keep(outcome: Outcome) -> Self {
        Self {
            outcome,
            transport: TransportPolicy::Keep,
        }
    }
}

/// Ties the key grammar to a live session. One `Engine` per open timeline.
#[derive(Debug)]
pub struct Engine {
    keymap: Keymap,
    parser: Parser,
    mode: ModeState,
    jump_cfg: JumpConfig,
    zoom: Zoom,
    /// Registers named with `"<reg>`; distinct from the
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

    /// What the user has selected, as a model-level [`Selection`] the host
    /// can act on. `None` outside visual mode: a command with no
    /// selection falls back to the playhead, and "no selection" must not be
    /// confused with "an empty selection".
    #[must_use]
    pub fn selection(&self) -> Option<Selection> {
        let sel = self.mode.visual()?;
        let range = sel.range();
        Some(Selection::new(range.start, range.end, sel.tracks.clone()))
    }

    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom;
    }

    /// Return to `NORMAL` with nothing pending, for a host that has switched
    /// to a different timeline. Registers survive, because they are
    /// them global across open timelines; a selection and a half-typed
    /// sequence do not, because they name positions in the timeline that is
    /// being left.
    pub fn reset(&mut self) {
        self.parser.reset();
        self.mode.escape();
    }

    /// Feed one key through the grammar and, once a sequence completes, run
    /// it against `session`.
    pub fn feed(&mut self, key: Key, session: &mut Session) -> Feed {
        // Vim always lets a bare `q` stop an active recording, regardless of
        // what the grammar would otherwise make of it.
        if key == Key::Char('q')
            && !self.parser.is_pending()
            && session.macros().recording().is_some()
        {
            return Feed::keep(match session.macros_mut().stop() {
                Ok(r) => Outcome::MacroStopped(r),
                Err(e) => Outcome::Error(e.to_string()),
            });
        }
        if session.macros().recording().is_some() {
            session.macros_mut().push(key.to_token());
        }
        match self.parser.feed(key, &self.keymap, self.mode.mode()) {
            Step::Pending => Feed::keep(Outcome::Pending),
            Step::Cancelled => Feed::keep(Outcome::Mode(self.mode.escape())),
            Step::Invalid => Feed::keep(Outcome::Invalid),
            Step::Complete(action) => {
                // Read the policy before running: `execute` consumes the
                // action. A macro replay is `Interrupt` as a whole, so the
                // keys it replays need no separate accounting.
                let transport = action.transport_policy();
                let outcome = self.execute(action, session);
                Feed { outcome, transport }
            }
        }
    }

    /// Owned rather than cached: [`JumpPointCache`] ties its return value's
    /// lifetime to `&mut self`, which cannot coexist with the `&Timeline`
    /// borrows every caller here also needs. Caching is worth restoring once
    /// a frontend actually calls this on a hot path.
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
    /// edit goes through here, so plugin edits are ordinary
    /// commands with ordinary undo.
    pub fn execute_action(&mut self, action: Action, session: &mut Session) -> Outcome {
        self.execute(action, session)
    }

    fn execute(&mut self, action: Action, session: &mut Session) -> Outcome {
        // A registered object cannot be resolved here: it needs Lua, which
        // lives above this crate. Hand the whole verb back to the host.
        if let Action::Verb {
            target: Target::Object(TextObject::Named { name, around }),
            ..
        } = action
        {
            return Outcome::ResolveObject {
                name,
                around,
                verb: Box::new(action),
            };
        }
        match action {
            Action::Move { motion, count } => self.do_move(&motion, count, session),
            Action::Verb {
                op,
                count,
                register,
                target,
            } => or_error(self.do_verb(op, count, register, &target, session)),
            Action::SplitCurrent => Self::do_split(false, session),
            Action::SplitAll => Self::do_split(true, session),
            Action::RippleDeleteClip => self.do_ripple_delete_clip(session),
            Action::Paste {
                before,
                ripple,
                register,
            } => self.do_paste(before, ripple, register, session),
            Action::InsertMedia => do_insert_media(session),
            Action::AppendMedia => Outcome::PickMedia(MediaIntent::Append),
            Action::Replace => do_replace(session),
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
            Action::SetMark(name) => do_set_mark(name, session),
            Action::JumpMark(name) => Self::do_jump_mark(name, session),
            Action::EnterVisual(kind) => {
                let p = session.timeline().playhead();
                let anchor = Anchor {
                    frame: p.frame,
                    track: p.track,
                };
                Outcome::Mode(self.mode.toggle_visual(kind, anchor))
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
            Action::NarrowSelection { group } => self.do_narrow_selection(group, session),
            Action::TrimEdgeStep { forward, count } => {
                or_error(self.do_trim_edge_step(forward, count, session))
            }
            Action::GainAdjust(step) => self.do_gain(step, session),
            Action::ToggleMute => Self::do_track_flags(session, true),
            Action::ToggleSolo => Self::do_track_flags(session, false),
            Action::CreateTransition => Self::do_transition(session, true),
            Action::DeleteTransition => Self::do_transition(session, false),
            Action::PlayPause => Outcome::Transport(TransportCmd::PlayPause),
            Action::Shuttle { forward } => Outcome::Transport(if forward {
                TransportCmd::ShuttleForward
            } else {
                TransportCmd::ShuttleBackward
            }),
            Action::ShuttleStop => Outcome::Transport(TransportCmd::ShuttleStop),
            Action::PreviewAndReturn => Outcome::Transport(TransportCmd::PreviewAndReturn),
            Action::LoopSelection => Outcome::Transport(TransportCmd::LoopSelection),
            Action::Zoom(intent) => Outcome::Zoom(intent),
            Action::Plugin { id, .. } => Outcome::Plugin(id),
            Action::InterruptTransport => Outcome::Transport(TransportCmd::Interrupt),
            Action::EnterCommandMode => Outcome::Mode(self.mode.enter(Mode::Command)),
            Action::Escape => Outcome::Mode(self.mode.escape()),
        }
    }

    fn do_move(&mut self, motion: &BuiltinMotion, count: u32, session: &mut Session) -> Outcome {
        let resolved = {
            let tl = session.timeline();
            let jumps = self.jump_points(tl);
            let mut ctx = MotionCtx::new(tl, &jumps);
            // In a VISUAL mode the end that moves is the selection's active
            // end, not the playhead, which stays where the selection was
            // anchored.
            if let Some(v) = self.mode.visual() {
                ctx = ctx.from(davimci_motion::Position {
                    frame: v.active.frame,
                    track: v.active.track,
                });
            }
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
                    Resolved::Pending | Resolved::Position(_) => Ok(None),
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
            // The host resolved this range itself; scope is the focused
            // track, as it is for every other single-track object.
            Target::Range(range) => Ok(Some((*range, Scope::single(playhead.track)))),
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
        target: &Target,
        session: &mut Session,
    ) -> Result<Outcome, KeysError> {
        match op {
            Operator::Yank => return self.do_yank(target, register, session),
            Operator::RippleTrim | Operator::Roll | Operator::Slip | Operator::Slide => {
                return self.do_edge_op(op, target, session);
            }
            Operator::Fade => return self.do_fade(target, session),
            _ => {}
        }
        let Some((range, scope)) = self.target_range(target, session)? else {
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
        if matches!(target, Target::Visual) {
            // A verb applied to a selection ends the selection, as in vim -
            // yank included, or the next motion silently extends a selection
            // the user believes is gone.
            self.mode.escape();
        }
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

    fn do_split(all_tracks: bool, session: &mut Session) -> Outcome {
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

    /// `it` / `at` with a selection live: keep the focused track, or the
    /// focused track and everything its link group reaches.
    ///
    /// The range is untouched - an object typed in VISUAL changes *scope*,
    /// which is the whole reason the objects carry one.
    fn do_narrow_selection(&mut self, group: bool, session: &Session) -> Outcome {
        if self.mode.visual().is_none() {
            return Outcome::Invalid;
        }
        let tl = session.timeline();
        let head = tl.playhead();
        let mut tracks = vec![head.track];
        if group
            && let Some(g) = tl
                .track(head.track)
                .and_then(|t| t.clip_at(head.frame))
                .and_then(|c| c.group)
        {
            tracks.extend(tl.group_members(g).into_iter().map(|(t, _)| t));
            tracks.dedup();
        }
        self.mode.set_visual_tracks(tracks);
        Outcome::Moved
    }

    /// `<` / `>`: ripple-trim the nearest edge by `count` jump points
    ///.
    ///
    /// Same command as `t` + motion; only the landing position is decided
    /// differently - by the jump-point set rather than by a typed motion, so
    /// the step is whatever the current zoom calls one.
    fn do_trim_edge_step(
        &mut self,
        forward: bool,
        count: u32,
        session: &mut Session,
    ) -> Result<Outcome, KeysError> {
        let playhead = session.timeline().playhead();
        let track = playhead.track;
        let (clip, edge) = nearest_edge(session.timeline(), track, playhead.frame)
            .ok_or(KeysError::EmptyTarget)?;
        let anchor =
            edge_frame(session.timeline(), track, clip, edge).ok_or(KeysError::EmptyTarget)?;
        let direction = if forward {
            davimci_motion::Direction::Forward
        } else {
            davimci_motion::Direction::Backward
        };
        let jumps = self.jump_points(session.timeline());
        // No jump point that way is the timeline's edge: nothing to trim to,
        // and a user error rather than a silent no-op.
        let to = jumps
            .step(anchor, direction, count.max(1))
            .ok_or(KeysError::EmptyTarget)?;
        Ok(run(session.exec(&EditCommand::Trim {
            track,
            clip,
            edge,
            delta: delta_of(anchor, to),
        })))
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

    /// `+` / `-`: adjust gain on the selection, or on the clip under the
    /// playhead when nothing is selected.
    ///
    /// A selection spanning several clips is one `Sequence`, so one `u`
    /// undoes the whole adjustment rather than one clip of it.
    fn do_gain(&mut self, step_db: i32, session: &mut Session) -> Outcome {
        // A gain step is a handful of dB; clamping first keeps the
        // conversion exact instead of silently rounding a nonsense value.
        let step = f32::from(i16::try_from(step_db).unwrap_or(i16::MAX));
        let cmds: Vec<EditCommand> = if let Some(sel) = self.selection() {
            sel.clips(session.timeline())
                .into_iter()
                .map(|(track, c)| EditCommand::SetProps {
                    track,
                    clip: c.id,
                    props: ClipProps {
                        gain_db: c.props.gain_db + step,
                        ..c.props
                    },
                })
                .collect()
        } else {
            let p = session.timeline().playhead();
            let Some(clip) = clip_under(session.timeline(), p.track, p.frame) else {
                return Outcome::Error("no clip under the playhead".to_string());
            };
            let Some((_, c)) = session.timeline().find_clip(clip) else {
                return Outcome::Error("no such clip".to_string());
            };
            vec![EditCommand::SetProps {
                track: p.track,
                clip,
                props: ClipProps {
                    gain_db: c.props.gain_db + step,
                    ..c.props
                },
            }]
        };
        if cmds.is_empty() {
            return Outcome::Error("no clip in the selection".to_string());
        }
        run(session.exec(&EditCommand::Sequence(cmds)))
    }

    /// Toggle mute or solo on the track the playhead is on.
    ///
    /// Mute and solo are independent flags: soloing a muted track leaves it
    /// muted, because silencing something is a stronger statement than
    /// featuring it and undoing the solo must not unmute by accident.
    fn do_track_flags(session: &mut Session, mute: bool) -> Outcome {
        let track = session.timeline().playhead().track;
        let Some(t) = session.timeline().track(track) else {
            return Outcome::Error("the playhead is not on a track".to_string());
        };
        let (name, muted, solo) = (t.name.clone(), t.muted, t.solo);
        let cmd = if mute {
            EditCommand::SetTrackFlags {
                track,
                muted: !muted,
                solo,
            }
        } else {
            EditCommand::SetTrackFlags {
                track,
                muted,
                solo: !solo,
            }
        };
        let state = match (mute, muted, solo) {
            (true, false, _) => "muted",
            (true, true, _) => "unmuted",
            (false, _, false) => "soloed",
            (false, _, true) => "unsoloed",
        };
        match run(session.exec(&cmd)) {
            Outcome::Applied(_) => Outcome::Applied(format!("{name} {state}")),
            other => other,
        }
    }

    /// `gx` and `dax`: put a default transition on the nearest cut, or take
    /// the one there away.
    ///
    /// "Nearest cut" rather than "the cut under the playhead": a transition
    /// straddles its cut, so demanding the playhead sit exactly on it would
    /// make `dax` unusable from inside the transition it deletes.
    fn do_transition(session: &mut Session, create: bool) -> Outcome {
        let playhead = session.timeline().playhead();
        let track = playhead.track;
        let found = if create {
            session.timeline().nearest_cut(track, playhead.frame)
        } else {
            session
                .timeline()
                .transition_at(track, playhead.frame)
                .map(|(clip, _)| (clip, playhead.frame))
        };
        let Some((clip, _)) = found else {
            return Outcome::Error(if create {
                "there is no cut on this track to put a transition on".to_string()
            } else {
                "there is no transition here to delete".to_string()
            });
        };
        let transition = create.then(Transition::dissolve);
        let cmd = EditCommand::SetTransition {
            track,
            clip,
            transition,
        };
        match run(session.exec(&cmd)) {
            Outcome::Applied(_) if create => Outcome::Applied(format!(
                "{DEFAULT_TRANSITION_FRAMES}-frame {DEFAULT_TRANSITION} added"
            )),
            Outcome::Applied(_) => Outcome::Applied("transition removed".to_string()),
            other => other,
        }
    }

    fn do_jump_mark(name: char, session: &mut Session) -> Outcome {
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
                    out.push(self.feed(key, session).outcome);
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

/// A refusal from a verb is a status line, not a failure of the engine.
fn or_error(result: Result<Outcome, KeysError>) -> Outcome {
    result.unwrap_or_else(|e| Outcome::Error(e.user_message_pub()))
}

/// `i` means two different things by context: on a text track it edits the
/// subtitle under the playhead, anywhere else it inserts media.
fn do_insert_media(session: &Session) -> Outcome {
    match text_clip_under_playhead(session) {
        Some((clip, text)) => Outcome::EditText { clip, text },
        None => Outcome::PickMedia(MediaIntent::Insert),
    }
}

/// Replace needs something to replace; refusing here means the picker never
/// opens for an edit that cannot land.
fn do_replace(session: &Session) -> Outcome {
    let head = session.timeline().playhead();
    if clip_under(session.timeline(), head.track, head.frame).is_some() {
        Outcome::PickMedia(MediaIntent::Replace)
    } else {
        Outcome::Error("there is no clip under the playhead to replace".to_string())
    }
}

fn do_set_mark(name: char, session: &mut Session) -> Outcome {
    let p = session.timeline().playhead();
    session.set_mark(name, p.frame, Some(p.track));
    Outcome::Applied(format!("mark '{name}' set at {}", p.frame))
}

fn interior(bounds: impl Iterator<Item = (Frame, Frame)>, frame: Frame) -> bool {
    bounds.into_iter().any(|(s, e)| frame > s && frame < e)
}

fn clip_under(tl: &Timeline, track: TrackId, frame: Frame) -> Option<davimci_core::ClipId> {
    tl.track(track)?.clip_at(frame).map(|c| c.id)
}

/// The subtitle clip under the playhead, if the focused track is a text one.
fn text_clip_under_playhead(session: &Session) -> Option<(ClipId, String)> {
    let tl = session.timeline();
    let head = tl.playhead();
    let track = tl.track(head.track)?;
    if track.kind != davimci_core::TrackKind::Text {
        return None;
    }
    let clip = track.clip_at(head.frame)?;
    Some((clip.id, clip.text.clone().unwrap_or_default()))
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
