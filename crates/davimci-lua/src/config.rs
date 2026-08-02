//! `davimci.timeline.configure` (spec §9.6): the jump-point and frame-step
//! settings a config may change.
//!
//! The output is a [`JumpConfig`] - the same type `davimci-motion` already
//! consumes - so a configured editor and a default one differ by data, never
//! by code path.

use davimci_keys::Key;
use davimci_motion::{JumpConfig, JumpSources};

use crate::error::LuaError;

/// Everything `davimci.timeline.configure` can set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineConfig {
    pub jump: JumpConfig,
    /// Keys bound to a one-frame step (spec §9.6 keeps these remappable but
    /// always frame-accurate).
    pub frame_step_keys: Vec<Vec<Key>>,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        Self {
            jump: JumpConfig::default(),
            frame_step_keys: vec![Key::parse_str("<Left>"), Key::parse_str("<Right>")],
        }
    }
}

impl TimelineConfig {
    /// Apply one `jump_points = { ... }` list. Every named source must be
    /// known: silently dropping a typo would leave the user staring at a
    /// timeline that does not do what their config says.
    pub(crate) fn set_sources(&mut self, names: &[String]) -> Result<(), LuaError> {
        let mut s = JumpSources {
            clip_bounds: false,
            markers: false,
            silence: false,
            peaks: false,
        };
        for n in names {
            match n.as_str() {
                "clip_bounds" => s.clip_bounds = true,
                "markers" => s.markers = true,
                "silence" => s.silence = true,
                "peaks" => s.peaks = true,
                other => {
                    return Err(LuaError::Config(format!(
                        "'{other}' is not a jump-point source (known: clip_bounds, markers, silence, peaks)"
                    )));
                }
            }
        }
        self.jump.sources = s;
        Ok(())
    }

    /// Apply `jump_point_density_per_zoom`. Only the zoom level at which
    /// dense subdivision begins is expressible in the engine today; the
    /// coarser entries describe the default behaviour and are accepted for
    /// forward compatibility, so a documented config keeps loading.
    pub(crate) fn set_density(&mut self, entries: &[(u8, String)]) -> Result<(), LuaError> {
        let mut dense_from: Option<u8> = None;
        for (level, kind) in entries {
            match kind.as_str() {
                "clip_bounds_only" | "clip_bounds+markers" => {}
                "dense_subdivision" => {
                    dense_from = Some(dense_from.map_or(*level, |l: u8| l.min(*level)));
                }
                other => {
                    return Err(LuaError::Config(format!(
                        "'{other}' is not a jump-point density (known: clip_bounds_only, clip_bounds+markers, dense_subdivision)"
                    )));
                }
            }
        }
        if let Some(l) = dense_from {
            self.jump.subdivide_from = l;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use davimci_core::Classify;

    #[test]
    fn sources_are_exactly_what_was_named() {
        let mut c = TimelineConfig::default();
        c.set_sources(&["clip_bounds".into(), "silence".into()])
            .unwrap();
        assert!(c.jump.sources.clip_bounds);
        assert!(c.jump.sources.silence);
        assert!(!c.jump.sources.markers);
        assert!(!c.jump.sources.peaks);
    }

    #[test]
    fn an_unknown_source_is_a_user_error_naming_the_alternatives() {
        let mut c = TimelineConfig::default();
        let e = c.set_sources(&["clip_bunds".into()]).expect_err("typo");
        assert!(e.user_message().contains("clip_bounds"), "{e}");
        // rejected before mutating: the default sources survive
        assert_eq!(c, TimelineConfig::default());
    }

    #[test]
    fn density_sets_the_subdivision_threshold_to_the_lowest_dense_level() {
        let mut c = TimelineConfig::default();
        c.set_density(&[
            (1, "clip_bounds_only".into()),
            (4, "clip_bounds+markers".into()),
            (10, "dense_subdivision".into()),
        ])
        .unwrap();
        assert_eq!(c.jump.subdivide_from, 10);
    }

    #[test]
    fn an_unknown_density_is_rejected() {
        let mut c = TimelineConfig::default();
        assert!(c.set_density(&[(1, "very_dense".into())]).is_err());
    }
}
