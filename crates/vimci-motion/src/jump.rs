//! The jump-point engine (spec §3.2).
//!
//! `h`/`l` do not move by a fixed distance: they move to the next *jump
//! point*, and the point set depends on the zoom level and on which sources
//! the user has enabled. Zoomed out the points are clip-level; zoomed in,
//! evenly spaced subdivisions appear between them.
//!
//! The set is pure: same timeline, same zoom, same config, same points. The
//! cache below exists only to avoid recomputation, and is keyed on a
//! fingerprint of everything the computation reads, so a stale hit is not
//! representable.

use vimci_core::{Frame, Timeline, TrackId};

use crate::target::Direction;

/// Zoom level. `0` is fully zoomed out (clip-level scrubbing); each step in
/// doubles the density of subdivisions once subdivisions begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Zoom(u8);

impl Zoom {
    pub const OUT: Self = Self(0);
    pub const MAX: Self = Self(16);

    /// Clamped constructor - a zoom level outside the range is pinned, never
    /// rejected, because zooming is not an operation that can fail.
    #[must_use]
    pub fn new(level: u8) -> Self {
        Self(level.min(Self::MAX.0))
    }

    #[must_use]
    pub fn level(self) -> u8 {
        self.0
    }

    #[must_use]
    pub fn zoom_in(self) -> Self {
        Self::new(self.0.saturating_add(1))
    }

    #[must_use]
    pub fn zoom_out(self) -> Self {
        Self(self.0.saturating_sub(1))
    }
}

impl Default for Zoom {
    fn default() -> Self {
        Self::OUT
    }
}

/// Which sources contribute jump points (spec §3.2, `jump_point_density`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpSources {
    pub clip_bounds: bool,
    pub markers: bool,
    /// Silence boundaries, supplied by the Phase 5 analysis index.
    pub silence: bool,
    /// Audio peaks, supplied by the Phase 5 analysis index.
    pub peaks: bool,
}

impl Default for JumpSources {
    fn default() -> Self {
        Self {
            clip_bounds: true,
            markers: true,
            silence: false,
            peaks: false,
        }
    }
}

/// Jump-point configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JumpConfig {
    pub sources: JumpSources,
    /// Zoom level at which evenly spaced subdivisions start appearing.
    pub subdivide_from: u8,
    /// Subdivision spacing in frames at `subdivide_from`. Halves per level in.
    pub base_spacing: u64,
}

impl Default for JumpConfig {
    fn default() -> Self {
        Self {
            sources: JumpSources::default(),
            subdivide_from: 2,
            base_spacing: 256,
        }
    }
}

impl JumpConfig {
    /// Subdivision spacing at `zoom`, or `None` when zoomed too far out for
    /// subdivisions to be useful.
    #[must_use]
    pub fn spacing(&self, zoom: Zoom) -> Option<Frame> {
        if zoom.level() < self.subdivide_from {
            return None;
        }
        let steps = u32::from(zoom.level() - self.subdivide_from);
        let spacing = self.base_spacing.checked_shr(steps).unwrap_or(0).max(1);
        Some(Frame(spacing))
    }
}

/// A sorted, deduplicated set of jump points.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JumpPoints {
    points: Vec<Frame>,
}

impl JumpPoints {
    /// Compute the point set for one track at one zoom level.
    ///
    /// `extra` carries analysis-derived points (silence edges, peaks) that
    /// `vimci-motion` cannot compute itself; they are filtered by the
    /// configured sources by the caller that owns the analysis index.
    #[must_use]
    pub fn build(
        tl: &Timeline,
        track: Option<TrackId>,
        zoom: Zoom,
        cfg: &JumpConfig,
        extra: &[Frame],
    ) -> Self {
        let mut points = vec![Frame::ZERO];
        let end = tl.duration();
        points.push(end);

        if cfg.sources.clip_bounds {
            for t in tl.tracks() {
                if track.is_some_and(|want| want != t.id) {
                    continue;
                }
                for c in t.clips() {
                    points.push(c.start);
                    points.push(c.end());
                }
            }
        }
        if cfg.sources.markers {
            points.extend(tl.markers.iter().map(|m| m.frame));
        }
        if cfg.sources.silence || cfg.sources.peaks {
            points.extend_from_slice(extra);
        }
        if let Some(spacing) = cfg.spacing(zoom) {
            let step = spacing.get().max(1);
            let mut f = 0u64;
            while f <= end.get() {
                points.push(Frame(f));
                f = f.saturating_add(step);
            }
        }

        points.retain(|f| *f <= end);
        points.sort_unstable();
        points.dedup();
        Self { points }
    }

    #[must_use]
    pub fn points(&self) -> &[Frame] {
        &self.points
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The nearest point strictly after `from`.
    ///
    /// The set is sorted and deduplicated, so this is a binary search: `l`
    /// must stay O(log n) even with a dense subdivision set (spec §3.2).
    #[must_use]
    pub fn next(&self, from: Frame) -> Option<Frame> {
        let i = self.points.partition_point(|p| *p <= from);
        self.points.get(i).copied()
    }

    /// The nearest point strictly before `from`.
    #[must_use]
    pub fn prev(&self, from: Frame) -> Option<Frame> {
        let i = self.points.partition_point(|p| *p < from);
        i.checked_sub(1).and_then(|i| self.points.get(i).copied())
    }

    /// `count` points away, saturating at the ends of the set.
    ///
    /// Saturating rather than failing matches vim: `100l` at the end of the
    /// timeline lands on the last point, it does not refuse to move.
    #[must_use]
    pub fn step(&self, from: Frame, dir: Direction, count: u32) -> Option<Frame> {
        // Stepping n points is index arithmetic on a sorted set, not n
        // searches: `1000l` costs the same as `l`.
        let count = usize::try_from(count.max(1)).unwrap_or(usize::MAX);
        match dir {
            Direction::Forward => {
                let first = self.points.partition_point(|p| *p <= from);
                if first >= self.points.len() {
                    return None;
                }
                let i = first.saturating_add(count - 1).min(self.points.len() - 1);
                self.points.get(i).copied()
            }
            Direction::Backward => {
                let after = self.points.partition_point(|p| *p < from);
                let first = after.checked_sub(1)?;
                self.points.get(first.saturating_sub(count - 1)).copied()
            }
        }
    }
}

/// Fingerprint of everything [`JumpPoints::build`] reads.
///
/// A cheap FNV-1a over clip bounds and markers. It is a cache key, never a
/// correctness guarantee for anything else.
#[must_use]
fn fingerprint(tl: &Timeline, track: Option<TrackId>, extra: &[Frame]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x100_0000_01b3);
    };
    eat(track.map_or(0, |t| t.get().wrapping_add(1)));
    for t in tl.tracks() {
        if track.is_some_and(|want| want != t.id) {
            continue;
        }
        eat(t.id.get());
        for c in t.clips() {
            eat(c.start.get());
            eat(c.end().get());
        }
    }
    for m in &tl.markers {
        eat(m.frame.get() ^ 0x5555);
    }
    for f in extra {
        eat(f.get() ^ 0xaaaa);
    }
    eat(tl.duration().get());
    h
}

/// Memoises one point set, invalidated by timeline or zoom change.
#[derive(Debug, Clone, Default)]
pub struct JumpPointCache {
    key: Option<(u64, Zoom, JumpConfig)>,
    points: JumpPoints,
}

impl JumpPointCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the cached set, recomputing if anything it depends on changed.
    pub fn get(
        &mut self,
        tl: &Timeline,
        track: Option<TrackId>,
        zoom: Zoom,
        cfg: &JumpConfig,
        extra: &[Frame],
    ) -> &JumpPoints {
        let key = (fingerprint(tl, track, extra), zoom, *cfg);
        if self.key.as_ref() != Some(&key) {
            self.points = JumpPoints::build(tl, track, zoom, cfg, extra);
            self.key = Some(key);
        }
        &self.points
    }

    /// Drop the memo. Callers that cannot fingerprint their inputs (a Lua
    /// jump-point source, say) invalidate explicitly.
    pub fn clear(&mut self) {
        self.key = None;
        self.points = JumpPoints::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vimci_core::testing::{fixture, track_id};
    use vimci_core::{Marker, TimelineProps};

    fn tl() -> Timeline {
        fixture(&[
            ("V1", &[(0, 100, "a"), (100, 150, "b")]),
            ("A1", &[(0, 40, "x")]),
        ])
    }

    #[test]
    fn clip_bounds_and_ends_are_points() {
        let tl = tl();
        let v1 = track_id(&tl, "V1");
        let jp = JumpPoints::build(&tl, Some(v1), Zoom::OUT, &JumpConfig::default(), &[]);
        assert_eq!(jp.points(), [Frame(0), Frame(100), Frame(250)]);
        assert!(!jp.is_empty());
    }

    #[test]
    fn all_tracks_contribute_when_no_track_is_named() {
        let tl = tl();
        let jp = JumpPoints::build(&tl, None, Zoom::OUT, &JumpConfig::default(), &[]);
        assert_eq!(jp.points(), [Frame(0), Frame(40), Frame(100), Frame(250)]);
    }

    #[test]
    fn markers_are_a_source_and_can_be_switched_off() {
        let mut tl = tl();
        tl.markers.push(Marker {
            frame: Frame(33),
            label: "m".into(),
        });
        let v1 = track_id(&tl, "V1");
        let mut cfg = JumpConfig::default();
        let with = JumpPoints::build(&tl, Some(v1), Zoom::OUT, &cfg, &[]);
        assert!(with.points().contains(&Frame(33)));
        cfg.sources.markers = false;
        let without = JumpPoints::build(&tl, Some(v1), Zoom::OUT, &cfg, &[]);
        assert!(!without.points().contains(&Frame(33)));
    }

    #[test]
    fn analysis_points_only_apply_when_their_source_is_enabled() {
        let tl = tl();
        let v1 = track_id(&tl, "V1");
        let mut cfg = JumpConfig::default();
        let off = JumpPoints::build(&tl, Some(v1), Zoom::OUT, &cfg, &[Frame(7)]);
        assert!(!off.points().contains(&Frame(7)));
        cfg.sources.silence = true;
        let on = JumpPoints::build(&tl, Some(v1), Zoom::OUT, &cfg, &[Frame(7)]);
        assert!(on.points().contains(&Frame(7)));
    }

    #[test]
    fn points_never_run_past_the_timeline() {
        let tl = tl();
        let cfg = JumpConfig::default();
        let jp = JumpPoints::build(&tl, None, Zoom::MAX, &cfg, &[Frame(9_999)]);
        assert!(jp.points().iter().all(|p| *p <= tl.duration()));
    }

    /// spec §3.2: denser as you zoom in, and never sparser.
    #[test]
    fn density_is_monotonic_in_zoom() {
        let tl = tl();
        let cfg = JumpConfig::default();
        let mut last = 0usize;
        for level in 0..=Zoom::MAX.level() {
            let n = JumpPoints::build(&tl, None, Zoom::new(level), &cfg, &[]).len();
            assert!(
                n >= last,
                "zoom {level} thinned the point set: {n} < {last}"
            );
            last = n;
        }
        // Fully zoomed in is frame-level.
        assert_eq!(last as u64, tl.duration().get() + 1);
    }

    #[test]
    fn the_point_set_is_deterministic() {
        let tl = tl();
        let cfg = JumpConfig::default();
        for _ in 0..4 {
            assert_eq!(
                JumpPoints::build(&tl, None, Zoom::new(5), &cfg, &[]),
                JumpPoints::build(&tl, None, Zoom::new(5), &cfg, &[])
            );
        }
    }

    #[test]
    fn stepping_saturates_at_the_ends() {
        let tl = tl();
        let v1 = track_id(&tl, "V1");
        let jp = JumpPoints::build(&tl, Some(v1), Zoom::OUT, &JumpConfig::default(), &[]);
        assert_eq!(jp.step(Frame(0), Direction::Forward, 1), Some(Frame(100)));
        assert_eq!(jp.step(Frame(0), Direction::Forward, 2), Some(Frame(250)));
        assert_eq!(jp.step(Frame(0), Direction::Forward, 99), Some(Frame(250)));
        assert_eq!(jp.step(Frame(250), Direction::Forward, 1), None);
        assert_eq!(
            jp.step(Frame(250), Direction::Backward, 1),
            Some(Frame(100))
        );
        assert_eq!(jp.step(Frame(0), Direction::Backward, 1), None);
        // A zero count behaves as one, as vim's counts do.
        assert_eq!(jp.step(Frame(0), Direction::Forward, 0), Some(Frame(100)));
    }

    /// `step` is index arithmetic rather than repeated `next`/`prev`; it must
    /// still agree with the naive walk everywhere, including a huge count.
    #[test]
    fn stepping_agrees_with_walking_one_point_at_a_time() {
        let tl = tl();
        let cfg = JumpConfig::default();
        let jp = JumpPoints::build(&tl, None, Zoom::new(4), &cfg, &[Frame(37), Frame(211)]);
        let walk = |from: Frame, dir: Direction, count: u32| {
            let mut at = from;
            let mut moved = None;
            for _ in 0..count.max(1) {
                match match dir {
                    Direction::Forward => jp.next(at),
                    Direction::Backward => jp.prev(at),
                } {
                    Some(f) => {
                        at = f;
                        moved = Some(f);
                    }
                    None => break,
                }
            }
            moved
        };
        for from in 0..=tl.duration().get() {
            for count in [0, 1, 2, 3, 7, 1000, u32::MAX] {
                for dir in [Direction::Forward, Direction::Backward] {
                    assert_eq!(
                        jp.step(Frame(from), dir, count),
                        walk(Frame(from), dir, count),
                        "from {from}, {dir:?} x{count}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_cache_recomputes_when_the_timeline_or_zoom_changes() {
        let mut tl = tl();
        let mut cache = JumpPointCache::new();
        let cfg = JumpConfig::default();
        let first = cache.get(&tl, None, Zoom::OUT, &cfg, &[]).clone();
        assert_eq!(*cache.get(&tl, None, Zoom::OUT, &cfg, &[]), first);

        let zoomed = cache.get(&tl, None, Zoom::new(4), &cfg, &[]).clone();
        assert_ne!(zoomed, first);

        tl.markers.push(Marker {
            frame: Frame(11),
            label: "m".into(),
        });
        let after = cache.get(&tl, None, Zoom::OUT, &cfg, &[]).clone();
        assert!(after.points().contains(&Frame(11)));

        cache.clear();
        assert_eq!(*cache.get(&tl, None, Zoom::OUT, &cfg, &[]), after);
    }

    #[test]
    fn an_empty_timeline_still_has_the_origin() {
        let tl = Timeline::new(TimelineProps::default());
        let jp = JumpPoints::build(&tl, None, Zoom::OUT, &JumpConfig::default(), &[]);
        assert_eq!(jp.points(), [Frame(0)]);
        assert_eq!(jp.next(Frame(0)), None);
        assert_eq!(jp.prev(Frame(0)), None);
    }
}
