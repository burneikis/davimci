//! Preview scale that follows the frame budget.
//!
//! Scrubbing already drops resolution rather than frames; playback should
//! too. This is the policy that decides when, expressed as a pure function of
//! the pacing counters so it is provable with no backend, no window and no
//! media: hand it [`PaceStats`] once per playback tick and it answers with
//! the scale to switch to, or nothing.
//!
//! Two properties matter more than the exact thresholds. It must not
//! oscillate - a reduction is only reconsidered after a full clean window -
//! and it must never reduce a scale the *user* chose, only one it chose
//! itself.

use davimci_backend::PreviewScale;

use crate::pacing::PaceStats;

/// Ticks in one decision window. At 60fps this is a second of playback,
/// which is long enough that a single hitch cannot drop the resolution and
/// short enough that a genuinely overloaded preview recovers quickly.
const WINDOW: u64 = 60;

/// Drops per window that count as "not keeping up", as a percentage of the
/// frames presented in the same window.
const DROP_PERCENT: u64 = 20;

/// The adaptive scale policy for one preview.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveScale {
    /// Counters at the start of the current window.
    baseline: PaceStats,
    ticks: u64,
    /// How many steps down this policy has taken. Only steps it took are
    /// ever given back, so `:set` - or a small window - keeps its scale.
    steps_down: u8,
}

/// What the policy decided, so the caller has a sentence to show without
/// inventing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaleChange {
    pub scale: PreviewScale,
    pub reduced: bool,
}

impl ScaleChange {
    /// One complete sentence for the status line.
    #[must_use]
    pub fn message(self) -> String {
        let what = match self.scale {
            PreviewScale::Full => "full resolution",
            PreviewScale::Half => "half resolution",
            PreviewScale::Quarter => "quarter resolution",
        };
        if self.reduced {
            format!("Preview dropped to {what} to keep up with the clock.")
        } else {
            format!("Preview is back to {what}.")
        }
    }
}

impl AdaptiveScale {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget the current window. Called when playback starts or stops, so a
    /// window never straddles two passes.
    pub fn reset(&mut self, stats: PaceStats) {
        self.baseline = stats;
        self.ticks = 0;
    }

    /// Give back everything this policy took, for a session that stops
    /// playing: the next pass starts at the scale the user asked for.
    pub fn release(&mut self, current: PreviewScale) -> Option<ScaleChange> {
        if self.steps_down == 0 {
            return None;
        }
        let mut scale = current;
        for _ in 0..self.steps_down {
            scale = up(scale);
        }
        self.steps_down = 0;
        (scale != current).then_some(ScaleChange {
            scale,
            reduced: false,
        })
    }

    /// One playback tick. Returns the scale to switch to, if the window just
    /// closed on a decision.
    pub fn observe(&mut self, stats: PaceStats, current: PreviewScale) -> Option<ScaleChange> {
        self.ticks += 1;
        if self.ticks < WINDOW {
            return None;
        }
        let dropped = stats
            .dropped_late
            .saturating_sub(self.baseline.dropped_late);
        let presented = stats.presented.saturating_sub(self.baseline.presented);
        self.reset(stats);

        let struggling = presented > 0 && dropped * 100 > presented * DROP_PERCENT;
        if struggling {
            let next = down(current);
            if next == current {
                return None;
            }
            self.steps_down = self.steps_down.saturating_add(1);
            return Some(ScaleChange {
                scale: next,
                reduced: true,
            });
        }
        // Only a window with no drops at all gives resolution back: a window
        // that is merely under the threshold is one that is already at its
        // limit, and stepping up would start the cycle again.
        if dropped == 0 && self.steps_down > 0 {
            self.steps_down -= 1;
            let next = up(current);
            if next != current {
                return Some(ScaleChange {
                    scale: next,
                    reduced: false,
                });
            }
        }
        None
    }
}

fn down(scale: PreviewScale) -> PreviewScale {
    match scale {
        PreviewScale::Full => PreviewScale::Half,
        PreviewScale::Half | PreviewScale::Quarter => PreviewScale::Quarter,
    }
}

fn up(scale: PreviewScale) -> PreviewScale {
    match scale {
        PreviewScale::Quarter => PreviewScale::Half,
        PreviewScale::Half | PreviewScale::Full => PreviewScale::Full,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn stats(presented: u64, dropped: u64) -> PaceStats {
        PaceStats {
            presented,
            dropped_late: dropped,
            repeated: 0,
        }
    }

    /// Feed `windows` full windows whose every tick presents one frame and
    /// drops `drop_per_tick`.
    fn run(
        policy: &mut AdaptiveScale,
        scale: &mut PreviewScale,
        acc: &mut PaceStats,
        windows: u64,
        drop_per_tick: u64,
    ) -> Vec<ScaleChange> {
        let mut out = Vec::new();
        for _ in 0..windows * WINDOW {
            acc.presented += 1;
            acc.dropped_late += drop_per_tick;
            if let Some(change) = policy.observe(*acc, *scale) {
                *scale = change.scale;
                out.push(change);
            }
        }
        out
    }

    #[test]
    fn a_clean_window_never_changes_the_scale() {
        let mut policy = AdaptiveScale::new();
        let mut scale = PreviewScale::Full;
        let mut acc = stats(0, 0);
        assert!(run(&mut policy, &mut scale, &mut acc, 4, 0).is_empty());
        assert_eq!(scale, PreviewScale::Full);
    }

    #[test]
    fn sustained_drops_step_down_one_level_per_window() {
        let mut policy = AdaptiveScale::new();
        let mut scale = PreviewScale::Full;
        let mut acc = stats(0, 0);
        let changes = run(&mut policy, &mut scale, &mut acc, 2, 1);
        assert_eq!(changes.len(), 2, "one step per window, not per tick");
        assert!(changes.iter().all(|c| c.reduced));
        assert_eq!(scale, PreviewScale::Quarter);
        // Quarter is the floor: it cannot report a change it cannot make.
        assert!(run(&mut policy, &mut scale, &mut acc, 2, 1).is_empty());
    }

    #[test]
    fn catching_up_restores_only_what_the_policy_took() {
        let mut policy = AdaptiveScale::new();
        let mut scale = PreviewScale::Full;
        let mut acc = stats(0, 0);
        run(&mut policy, &mut scale, &mut acc, 1, 1);
        assert_eq!(scale, PreviewScale::Half);
        let back = run(&mut policy, &mut scale, &mut acc, 1, 0);
        assert_eq!(scale, PreviewScale::Full);
        assert_eq!(back.len(), 1);
        assert!(!back[0].reduced);
        assert!(back[0].message().ends_with('.'));
        // Nothing left to give back, so a further clean window is silent.
        assert!(run(&mut policy, &mut scale, &mut acc, 2, 0).is_empty());
    }

    #[test]
    fn a_scale_the_policy_did_not_take_is_never_given_back() {
        let mut policy = AdaptiveScale::new();
        let mut scale = PreviewScale::Quarter;
        let mut acc = stats(0, 0);
        assert!(run(&mut policy, &mut scale, &mut acc, 3, 0).is_empty());
        assert_eq!(scale, PreviewScale::Quarter, "the user's scale was raised");
        assert!(policy.release(scale).is_none());
    }

    #[test]
    fn stopping_playback_gives_back_every_step_at_once() {
        let mut policy = AdaptiveScale::new();
        let mut scale = PreviewScale::Full;
        let mut acc = stats(0, 0);
        run(&mut policy, &mut scale, &mut acc, 2, 1);
        assert_eq!(scale, PreviewScale::Quarter);
        let released = policy.release(scale).unwrap();
        assert_eq!(released.scale, PreviewScale::Full);
        assert!(!released.reduced);
        assert!(policy.release(released.scale).is_none());
    }

    #[test]
    fn a_short_hitch_inside_a_window_is_not_a_reduction() {
        let mut policy = AdaptiveScale::new();
        let mut acc = stats(0, 0);
        // One window, one hitch: 1 drop against 60 presented is under the
        // threshold, so the picture keeps its resolution.
        for i in 0..WINDOW {
            acc.presented += 1;
            if i == 10 {
                acc.dropped_late += 1;
            }
            assert!(policy.observe(acc, PreviewScale::Full).is_none());
        }
    }
}
