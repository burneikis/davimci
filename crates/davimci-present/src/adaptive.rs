//! Preview scale that follows the frame budget.
//!
//! Scrubbing already drops resolution rather than frames; playback should
//! too. This is the policy that decides when, expressed as a pure function of
//! the pacing counters so it is provable with no backend, no window and no
//! media: hand it [`PaceStats`] once per playback tick and it answers with
//! the scale to switch to, or nothing.
//!
//! Two properties matter more than the exact thresholds. It must not
//! oscillate - taking a step back up costs a consumer restart, so it needs
//! `CLEAN_WINDOWS` spotless windows in a row, not one - and it must never
//! reduce a scale the *user* chose, only one it chose itself.

use davimci_backend::PreviewScale;

use crate::pacing::PaceStats;

/// Ticks in one decision window. At 60fps this is a second of playback,
/// which is long enough that a single hitch cannot drop the resolution and
/// short enough that a genuinely overloaded preview recovers quickly.
const WINDOW: u64 = 60;

/// Drops per window that count as "not keeping up", as a percentage of the
/// frames presented in the same window.
const DROP_PERCENT: u64 = 20;

/// Clean windows in a row before resolution is given back.
///
/// Every change of scale restarts the preview consumer, which is an audible
/// gap in the sound and a stalled picture. One clean window is not evidence
/// the machine can hold the higher scale, and acting on it makes playback
/// pause about once a second as the policy steps up and straight back down.
const CLEAN_WINDOWS: u8 = 3;

/// The adaptive scale policy for one preview.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveScale {
    /// Counters at the start of the current window.
    baseline: PaceStats,
    ticks: u64,
    /// How many steps down this policy has taken. Only steps it took are
    /// ever given back, so `:set` - or a small window - keeps its scale.
    steps_down: u8,
    /// Consecutive windows with no dropped frame.
    clean: u8,
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
        self.clean = 0;
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

        // Only a window with no drops at all counts as clean: a window that
        // is merely under the threshold is one already at its limit.
        self.clean = if dropped == 0 {
            self.clean.saturating_add(1)
        } else {
            0
        };

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
        if self.clean >= CLEAN_WINDOWS && self.steps_down > 0 {
            self.clean = 0;
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
        // A step up restarts the consumer, so it waits for `CLEAN_WINDOWS`
        // windows in a row rather than the first one.
        let early = run(
            &mut policy,
            &mut scale,
            &mut acc,
            u64::from(CLEAN_WINDOWS) - 1,
            0,
        );
        assert!(early.is_empty(), "one clean window is not evidence");
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

    /// Regression: a policy that stepped back up after a single clean window
    /// ping-ponged between two scales, and every change restarts the preview
    /// consumer - playback paused about once a second.
    #[test]
    fn alternating_windows_never_step_back_up() {
        let mut policy = AdaptiveScale::new();
        let mut scale = PreviewScale::Full;
        let mut acc = stats(0, 0);
        let mut changes = Vec::new();
        for _ in 0..8 {
            changes.extend(run(&mut policy, &mut scale, &mut acc, 1, 1));
            changes.extend(run(&mut policy, &mut scale, &mut acc, 1, 0));
        }
        assert!(
            changes.iter().all(|c| c.reduced),
            "a scale given back between two bad windows is a restart per second"
        );
        assert_eq!(scale, PreviewScale::Quarter);
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
