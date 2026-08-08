//! Motions.
//!
//! A motion answers "where does the playhead go", and nothing else: it never
//! mutates, so `d` + motion can resolve the target first and only then build
//! a command. Counts are applied by the motion, saturating at the timeline's
//! ends the way vim's do.

use davimci_core::{Frame, Timeline, TrackId};

use crate::error::MotionError;
use crate::jump::JumpPoints;
use crate::predicate::{Answer, NoAnalysis, Predicate, PredicateIndex};
use crate::target::{Direction, Position, Resolved};

const NO_ANALYSIS: NoAnalysis = NoAnalysis;

/// Everything a motion is allowed to read.
#[derive(Debug, Clone, Copy)]
pub struct MotionCtx<'a> {
    pub timeline: &'a Timeline,
    /// Jump points for the focused track at the current zoom, from
    /// [`crate::jump::JumpPointCache`].
    pub jumps: &'a JumpPoints,
    /// Analysis index backing predicate motions. Defaults to
    /// [`NoAnalysis`], which reports `Pending` for everything.
    pub analysis: &'a dyn PredicateIndex,
    /// Where the motion starts from, when that is not the playhead. In a
    /// `VISUAL*` mode the moving end is the selection's active end, so a
    /// motion resolved from the playhead would snap the selection back to
    /// where it was anchored instead of extending it.
    pub origin: Option<Position>,
}

impl<'a> MotionCtx<'a> {
    #[must_use]
    pub fn new(timeline: &'a Timeline, jumps: &'a JumpPoints) -> Self {
        Self {
            timeline,
            jumps,
            analysis: &NO_ANALYSIS,
            origin: None,
        }
    }

    /// Resolve from `origin` rather than from the playhead.
    #[must_use]
    pub fn from(mut self, origin: Position) -> Self {
        self.origin = Some(origin);
        self
    }

    #[must_use]
    pub fn with_analysis(mut self, analysis: &'a dyn PredicateIndex) -> Self {
        self.analysis = analysis;
        self
    }

    fn track(&self) -> TrackId {
        self.origin
            .map_or_else(|| self.timeline.playhead().track, |p| p.track)
    }

    fn frame(&self) -> Frame {
        self.origin
            .map_or_else(|| self.timeline.playhead().frame, |p| p.frame)
    }

    fn at(&self, frame: Frame) -> Resolved {
        Resolved::Position(Position {
            frame,
            track: self.track(),
        })
    }

    fn on(&self, track: TrackId) -> Resolved {
        Resolved::Position(Position {
            frame: self.frame(),
            track,
        })
    }

    /// Last addressable frame. An empty timeline has only frame zero.
    fn last_frame(&self) -> Frame {
        Frame(self.timeline.duration().get().saturating_sub(1))
    }

    fn track_index(&self) -> Option<usize> {
        let id = self.track();
        self.timeline.tracks().iter().position(|t| t.id == id)
    }
}

/// Anything that can resolve to a motion target.
///
/// Lua-defined motions (Phase 7) implement this too, which is why it is a
/// trait and not just the enum below.
pub trait Motion {
    /// Resolve against the current playhead. `count` of 0 means 1, as in vim.
    fn resolve(&self, ctx: &MotionCtx<'_>, count: u32) -> Result<Resolved, MotionError>;
}

/// The built-in motion set.
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinMotion {
    /// Arrow keys: exactly one frame, whatever the zoom.
    Frame(Direction),
    /// `h` / `l`: N jump points.
    JumpPoint(Direction),
    /// `j` / `k`: move track focus, clamped at the ends of the stack.
    TrackStep(Direction),
    /// `]t` / `[t`: cycle track focus, wrapping.
    TrackCycle(Direction),
    /// `w` / `b`: next or previous clip boundary on the focused track.
    ClipBoundary(Direction),
    /// `e`: last frame of the current clip, then of each following clip.
    ClipEnd,
    /// `0` / `gg`.
    TimelineStart,
    /// `$` / `G`.
    TimelineEnd,
    /// `{` / `}`: markers.
    Marker(Direction),
    /// `%`: the other end of the clip under the playhead.
    MatchingEdit,
    /// `` `a ``: jump to a mark.
    Mark(char),
    /// Predicate motions, answered by the analysis index.
    Predicate(Predicate, Direction),
}

impl BuiltinMotion {
    /// Which way along the timeline this motion travels, when it travels along
    /// it at all.
    ///
    /// A visual mode needs this to start the search from the edge of the
    /// selection it is about to push, rather than from the point the cursor
    /// happens to sit on inside it.
    #[must_use]
    pub fn time_direction(&self) -> Option<Direction> {
        match self {
            Self::Frame(dir)
            | Self::JumpPoint(dir)
            | Self::ClipBoundary(dir)
            | Self::Marker(dir)
            | Self::Predicate(_, dir) => Some(*dir),
            Self::ClipEnd | Self::TimelineEnd => Some(Direction::Forward),
            Self::TimelineStart => Some(Direction::Backward),
            Self::TrackStep(_) | Self::TrackCycle(_) | Self::MatchingEdit | Self::Mark(_) => None,
        }
    }
}

impl Motion for BuiltinMotion {
    fn resolve(&self, ctx: &MotionCtx<'_>, count: u32) -> Result<Resolved, MotionError> {
        let n = count.max(1);
        match self {
            Self::Frame(dir) => Ok(ctx.at(step_frames(ctx, *dir, n))),
            Self::JumpPoint(dir) => ctx
                .jumps
                .step(ctx.frame(), *dir, n)
                .map(|f| ctx.at(f))
                .ok_or(MotionError::NoJumpPoint),
            Self::TrackStep(dir) => step_track(ctx, *dir, n, false).map(|t| ctx.on(t)),
            Self::TrackCycle(dir) => step_track(ctx, *dir, n, true).map(|t| ctx.on(t)),
            Self::ClipBoundary(dir) => boundary(ctx, *dir, n).map(|f| ctx.at(f)),
            Self::ClipEnd => clip_end(ctx, n).map(|f| ctx.at(f)),
            Self::TimelineStart => Ok(ctx.at(Frame::ZERO)),
            Self::TimelineEnd => Ok(ctx.at(ctx.last_frame())),
            Self::Marker(dir) => marker(ctx, *dir, n).map(|f| ctx.at(f)),
            Self::MatchingEdit => matching_edit(ctx).map(|f| ctx.at(f)),
            Self::Mark(name) => ctx
                .timeline
                .marks
                .get(name)
                .map(|m| {
                    Resolved::Position(Position {
                        frame: m.frame,
                        track: m.track.unwrap_or_else(|| ctx.track()),
                    })
                })
                .ok_or(MotionError::NoSuchMark(*name)),
            Self::Predicate(p, dir) => predicate(ctx, p, *dir, n),
        }
    }
}

fn step_frames(ctx: &MotionCtx<'_>, dir: Direction, n: u32) -> Frame {
    let f = ctx.frame().get();
    let n = u64::from(n);
    match dir {
        Direction::Forward => Frame(f.saturating_add(n).min(ctx.last_frame().get())),
        Direction::Backward => Frame(f.saturating_sub(n)),
    }
}

fn step_track(
    ctx: &MotionCtx<'_>,
    dir: Direction,
    n: u32,
    wrap: bool,
) -> Result<TrackId, MotionError> {
    let tracks = ctx.timeline.tracks();
    if tracks.is_empty() {
        return Err(MotionError::NoTrackThere);
    }
    let here = ctx.track_index().ok_or(MotionError::NoTrackThere)?;
    let len = tracks.len();
    // Forward means "down the stack", matching `j`.
    let delta = (n as usize) % len.max(1);
    let idx = if wrap {
        match dir {
            Direction::Forward => (here + delta) % len,
            Direction::Backward => (here + len - delta) % len,
        }
    } else {
        let want = match dir {
            Direction::Forward => here.saturating_add(n as usize),
            Direction::Backward => here.saturating_sub(n as usize),
        }
        .min(len - 1);
        // Clamp first, then refuse: `j` on the bottom track is a no-op the
        // user should be told about, not a silent stay-put.
        if want == here {
            return Err(MotionError::NoTrackThere);
        }
        want
    };
    Ok(tracks[idx].id)
}

/// Every cut on a track: clip starts and clip ends, sorted.
fn boundaries(tl: &Timeline, track: TrackId) -> Result<Vec<Frame>, MotionError> {
    let t = tl
        .track(track)
        .ok_or_else(|| MotionError::NoSuchTrack(track.to_string()))?;
    let mut out: Vec<Frame> = t
        .clips()
        .iter()
        .flat_map(|c| [c.start, c.end()])
        .collect::<Vec<_>>();
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn boundary(ctx: &MotionCtx<'_>, dir: Direction, n: u32) -> Result<Frame, MotionError> {
    let bounds = boundaries(ctx.timeline, ctx.track())?;
    let mut at = ctx.frame();
    let mut moved = None;
    for _ in 0..n {
        let next = match dir {
            Direction::Forward => bounds.iter().copied().find(|b| *b > at),
            Direction::Backward => bounds.iter().rev().copied().find(|b| *b < at),
        };
        match next {
            Some(f) => {
                at = f;
                moved = Some(f);
            }
            None => break,
        }
    }
    moved.ok_or(MotionError::NoBoundary)
}

/// `e`: the last frame of the current clip, or of the next one if already
/// sitting on it - vim's `e` never stands still.
fn clip_end(ctx: &MotionCtx<'_>, n: u32) -> Result<Frame, MotionError> {
    let t = ctx
        .timeline
        .track(ctx.track())
        .ok_or_else(|| MotionError::NoSuchTrack(ctx.track().to_string()))?;
    let mut at = ctx.frame();
    let mut moved = None;
    for _ in 0..n {
        let next = t
            .clips()
            .iter()
            .map(|c| Frame(c.end().get().saturating_sub(1)))
            .find(|e| *e > at);
        match next {
            Some(f) => {
                at = f;
                moved = Some(f);
            }
            None => break,
        }
    }
    moved.ok_or(MotionError::NoBoundary)
}

fn marker(ctx: &MotionCtx<'_>, dir: Direction, n: u32) -> Result<Frame, MotionError> {
    let mut frames: Vec<Frame> = ctx.timeline.markers.iter().map(|m| m.frame).collect();
    frames.sort_unstable();
    frames.dedup();
    let mut at = ctx.frame();
    let mut moved = None;
    for _ in 0..n {
        let next = match dir {
            Direction::Forward => frames.iter().copied().find(|f| *f > at),
            Direction::Backward => frames.iter().rev().copied().find(|f| *f < at),
        };
        match next {
            Some(f) => {
                at = f;
                moved = Some(f);
            }
            None => break,
        }
    }
    moved.ok_or(MotionError::NoMarker)
}

/// `%`: hop between the two ends of the clip under the playhead.
fn matching_edit(ctx: &MotionCtx<'_>) -> Result<Frame, MotionError> {
    let frame = ctx.frame();
    let t = ctx
        .timeline
        .track(ctx.track())
        .ok_or_else(|| MotionError::NoSuchTrack(ctx.track().to_string()))?;
    let c = t
        .clip_at(frame)
        .ok_or(MotionError::NoMatchingEdit { frame: frame.get() })?;
    let last = Frame(c.end().get().saturating_sub(1));
    Ok(if frame == c.start { last } else { c.start })
}

fn predicate(
    ctx: &MotionCtx<'_>,
    p: &Predicate,
    dir: Direction,
    n: u32,
) -> Result<Resolved, MotionError> {
    let mut at = ctx.frame();
    let mut moved = None;
    for _ in 0..n {
        match ctx.analysis.find(p, at, dir) {
            Answer::Pending => return Ok(Resolved::Pending),
            Answer::NoMatch => break,
            Answer::Found(f) => {
                at = f;
                moved = Some(f);
            }
        }
    }
    moved
        .map(|f| {
            Resolved::Position(Position {
                frame: f,
                track: p.track(),
            })
        })
        .ok_or(MotionError::NoPredicateMatch)
}
