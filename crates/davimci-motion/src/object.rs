//! Text objects (spec §4.1).
//!
//! An object resolves to a range *and* a track scope, and the scope is the
//! whole point: it is the object, not the verb, that decides whether an edit
//! is track-local or follows link groups.
//!
//! - `ic` inner clip - the clip's own content, no transition, focused track.
//! - `ac` a clip - the clip plus its adjoining transitions. It equals `ic`
//!   until a transition is attached to one of the clip's cuts, and widens to
//!   cover the overlap once one is (spec §6.2).
//! - `it` inner track - the clip's extent, focused track only, link groups
//!   deliberately ignored.
//! - `at` a track group - the clip's extent across every track its link
//!   group reaches. An unlinked clip makes `at` equal to `it`.
//! - `is` inner segment - a sub-range set in VISUAL, focused track only.

use davimci_core::{Timeline, TrackId};

use crate::error::MotionError;
use crate::motion::MotionCtx;
use crate::target::{Resolved, Scope, TimeRange};

/// The built-in text objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextObject {
    InnerClip,
    AClip,
    InnerTrack,
    ATrack,
    /// Carries the visual selection; `None` outside VISUAL mode.
    InnerSegment(Option<TimeRange>),
}

/// Anything that resolves to a scoped range. Lua objects implement it too.
pub trait Object {
    fn resolve(&self, ctx: &MotionCtx<'_>) -> Result<Resolved, MotionError>;
}

impl Object for TextObject {
    fn resolve(&self, ctx: &MotionCtx<'_>) -> Result<Resolved, MotionError> {
        let tl = ctx.timeline;
        let track = tl.playhead().track;
        match self {
            Self::InnerSegment(seg) => {
                let range = seg.ok_or(MotionError::NoSegment)?;
                Ok(Resolved::Range(range, Scope::single(track)))
            }
            Self::InnerClip | Self::InnerTrack => {
                let (range, _) = clip_extent(tl, track)?;
                Ok(Resolved::Range(range, Scope::single(track)))
            }
            // `ac` covers the overlaps as well, so `dac` takes the clip and
            // the transitions that die with it rather than leaving handles
            // composited against nothing.
            Self::AClip => {
                let (range, _) = clip_extent(tl, track)?;
                let widened = tl
                    .track(track)
                    .and_then(|t| {
                        let clip = t.clip_at(tl.playhead().frame)?;
                        t.transition_range(clip.id)
                    })
                    .map_or(range, |(start, end)| TimeRange::new(start, end));
                Ok(Resolved::Range(widened, Scope::single(track)))
            }
            Self::ATrack => {
                let (range, group) = clip_extent(tl, track)?;
                let scope = match group {
                    None => Scope::single(track),
                    Some(g) => {
                        let mut tracks = vec![track];
                        tracks.extend(tl.group_members(g).into_iter().map(|(t, _)| t));
                        Scope::new(tracks)
                    }
                };
                Ok(Resolved::Range(range, scope))
            }
        }
    }
}

/// Extent and link group of the clip under the playhead on `track`.
fn clip_extent(
    tl: &Timeline,
    track: TrackId,
) -> Result<(TimeRange, Option<davimci_core::GroupId>), MotionError> {
    let t = tl
        .track(track)
        .ok_or_else(|| MotionError::NoSuchTrack(track.to_string()))?;
    let c = t
        .clip_at(tl.playhead().frame)
        .ok_or_else(|| MotionError::NoClipUnderPlayhead {
            track: t.name.clone(),
        })?;
    Ok((TimeRange::new(c.start, c.end()), c.group))
}
