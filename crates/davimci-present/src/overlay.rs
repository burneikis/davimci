//! The overlay model.
//!
//! The presenter describes overlays; it does not rasterise text. Text belongs
//! to the host's own text stack (`egui` glyphs, terminal cells), and forcing a
//! font rasteriser in here would give the GUI and TUI two different-looking
//! timecodes for the same frame.
//!
//! Overlays exist in [`crate::Host::Embedded`] only: the detached window is a
//! bare video surface behind a terminal, and drawing chrome on it would put
//! two status lines on screen.

use davimci_core::{Fps, Frame};

use crate::fit::Quad;

/// A rectangle drawn over the video, in surface pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayRect {
    pub quad: Quad,
    pub kind: OverlayRectKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayRectKind {
    /// 90% action-safe box.
    ActionSafe,
    /// 80% title-safe box.
    TitleSafe,
}

/// What a host should draw on top of the video quad this frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overlay {
    /// `HH:MM:SS:FF` at the presented position, or `None` when nothing is up.
    pub timecode: Option<String>,
    pub rects: Vec<OverlayRect>,
}

/// Which overlays are enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayConfig {
    pub timecode: bool,
    pub safe_areas: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            timecode: true,
            safe_areas: false,
        }
    }
}

impl OverlayConfig {
    #[must_use]
    pub fn none() -> Self {
        Self {
            timecode: false,
            safe_areas: false,
        }
    }

    #[must_use]
    pub fn build(self, position: Option<Frame>, fps: Fps, quad: Quad) -> Overlay {
        let mut overlay = Overlay::default();
        if self.timecode {
            overlay.timecode = position.map(|p| timecode(p, fps));
        }
        if self.safe_areas && quad.width > 0 && quad.height > 0 {
            overlay.rects.push(OverlayRect {
                quad: inset(quad, 90),
                kind: OverlayRectKind::ActionSafe,
            });
            overlay.rects.push(OverlayRect {
                quad: inset(quad, 80),
                kind: OverlayRectKind::TitleSafe,
            });
        }
        overlay
    }
}

fn inset(quad: Quad, percent: u32) -> Quad {
    let width = quad.width * percent / 100;
    let height = quad.height * percent / 100;
    Quad {
        x: quad.x + (quad.width - width) / 2,
        y: quad.y + (quad.height - height) / 2,
        width,
        height,
    }
}

/// `HH:MM:SS:FF`, non-drop-frame: davimci's model is whole frames at one rate
///, so there is no drop-frame case to represent.
#[must_use]
pub fn timecode(position: Frame, fps: Fps) -> String {
    let rate = fps.as_f64().round().max(1.0) as u64;
    let total = position.get();
    let frames = total % rate;
    let secs = total / rate;
    format!(
        "{:02}:{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60,
        frames
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad() -> Quad {
        Quad {
            x: 10,
            y: 20,
            width: 200,
            height: 100,
        }
    }

    #[test]
    fn timecode_rolls_over_at_the_frame_rate() {
        assert_eq!(timecode(Frame(0), Fps::FPS_60), "00:00:00:00");
        assert_eq!(timecode(Frame(59), Fps::FPS_60), "00:00:00:59");
        assert_eq!(timecode(Frame(60), Fps::FPS_60), "00:00:01:00");
        assert_eq!(timecode(Frame(3600 * 60), Fps::FPS_60), "01:00:00:00");
    }

    #[test]
    fn fractional_rates_round_to_their_nominal_rate() {
        // 23.976 counts 24 frames per timecode second, like every NLE.
        assert_eq!(timecode(Frame(24), Fps::FPS_23_976), "00:00:01:00");
    }

    #[test]
    fn safe_areas_are_centred_inside_the_quad() {
        let o = OverlayConfig {
            timecode: false,
            safe_areas: true,
        }
        .build(None, Fps::FPS_60, quad());
        assert_eq!(o.rects.len(), 2);
        let action = o.rects[0].quad;
        assert_eq!(action.width, 180);
        assert_eq!(action.x, 10 + 10);
        let title = o.rects[1].quad;
        assert!(title.width < action.width);
    }

    #[test]
    fn disabled_overlays_produce_nothing() {
        let o = OverlayConfig::none().build(Some(Frame(1)), Fps::FPS_60, quad());
        assert_eq!(o, Overlay::default());
    }
}
