//! The presenter: one video path, two hosts (plan.md Phase 9b).
//!
//! This is the anti-duplication keystone. Both frontends hand frames to the
//! same [`Presenter`], which paces them against the audio clock, letterboxes
//! them into its surface, and produces the composited RGBA image plus an
//! overlay model. The only thing [`Host`] changes is whether overlays are
//! described - the *video pixels are identical*, which the host-parity test
//! asserts directly.

use davimci_backend::{PreviewScale, RenderBackend, VideoFrame};
use davimci_core::{Fps, Frame, Resolution};

use crate::error::PresentError;
use crate::fit::{Quad, letterbox};
use crate::overlay::{Overlay, OverlayConfig};
use crate::pacing::{Pace, PaceStats, Pacer};

/// Where the video is being shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// Inside the GUI's own window, alongside `egui`. Overlays are allowed
    /// here and only here.
    Embedded,
    /// A bare, undecorated, non-focusable window for TUI mode, so the
    /// terminal never loses keyboard focus.
    Detached,
}

impl Host {
    /// Whether this host may draw chrome over the video.
    #[must_use]
    pub fn allows_overlay(self) -> bool {
        matches!(self, Host::Embedded)
    }
}

/// One composited image, ready for a host to upload as a texture (GUI) or
/// downsample into cells (TUI).
#[derive(Clone, PartialEq, Eq)]
pub struct Presentation {
    pub surface: Resolution,
    /// `surface.width * surface.height * 4`, RGBA8. Letterbox bars are
    /// opaque black.
    pub pixels: Vec<u8>,
    /// Where the video landed inside the surface.
    pub quad: Quad,
    /// The frame on screen, or `None` if nothing has been presented.
    pub position: Option<Frame>,
    pub overlay: Overlay,
    pub pace: Pace,
}

impl std::fmt::Debug for Presentation {
    /// Never dump the pixel buffer - a presentation in a log must stay
    /// readable, same rule as `VideoFrame`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presentation")
            .field("surface", &self.surface)
            .field("quad", &self.quad)
            .field("position", &self.position)
            .field("pace", &self.pace)
            .field("overlay", &self.overlay)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

impl Presentation {
    /// Pixel at `(x, y)`, or opaque black when out of bounds.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.surface.width || y >= self.surface.height {
            return [0, 0, 0, 255];
        }
        let i = ((y as usize) * (self.surface.width as usize) + (x as usize)) * 4;
        match self.pixels.get(i..i + 4) {
            Some(px) => [px[0], px[1], px[2], px[3]],
            None => [0, 0, 0, 255],
        }
    }
}

/// The shared video path.
#[derive(Debug)]
pub struct Presenter {
    host: Host,
    surface: Resolution,
    fps: Fps,
    scale: PreviewScale,
    overlay_cfg: OverlayConfig,
    pacer: Pacer,
}

impl Presenter {
    #[must_use]
    pub fn new(host: Host, surface: Resolution, fps: Fps) -> Self {
        Self {
            host,
            surface,
            fps,
            scale: PreviewScale::Full,
            overlay_cfg: OverlayConfig::default(),
            pacer: Pacer::new(),
        }
    }

    #[must_use]
    pub fn host(&self) -> Host {
        self.host
    }

    #[must_use]
    pub fn surface(&self) -> Resolution {
        self.surface
    }

    pub fn resize(&mut self, surface: Resolution) {
        self.surface = surface;
    }

    /// Preview decode scale. Scrubbing drops resolution rather than frames,
    /// and a small surface asks for a small decode by construction.
    #[must_use]
    pub fn scale(&self) -> PreviewScale {
        self.scale
    }

    pub fn set_scale(&mut self, scale: PreviewScale) {
        self.scale = scale;
    }

    /// Pick a decode scale that is no smaller than the surface needs. A
    /// quarter-res pull into a full-res window would be visibly soft, so the
    /// rule is one-directional: never decode below what is drawn.
    pub fn auto_scale(&mut self, source: Resolution) {
        let quad = letterbox(source, self.surface);
        self.scale = if quad.width * 4 <= source.width {
            PreviewScale::Quarter
        } else if quad.width * 2 <= source.width {
            PreviewScale::Half
        } else {
            PreviewScale::Full
        };
    }

    pub fn set_overlay_config(&mut self, cfg: OverlayConfig) {
        self.overlay_cfg = cfg;
    }

    #[must_use]
    pub fn stats(&self) -> PaceStats {
        self.pacer.stats()
    }

    #[must_use]
    pub fn current(&self) -> Option<&VideoFrame> {
        self.pacer.current()
    }

    pub fn clear(&mut self) {
        self.pacer.clear();
    }

    /// Present one tick of playback: pace against the clock, then compose.
    pub fn present(
        &mut self,
        backend: &mut dyn RenderBackend,
    ) -> Result<Presentation, PresentError> {
        let clock = backend.audio_clock_position();
        let pace = self.pacer.tick(clock, backend)?;
        self.compose(pace)
    }

    /// Show one frame outside playback - scrubbing, seeking, a still.
    pub fn present_frame(&mut self, frame: VideoFrame) -> Result<Presentation, PresentError> {
        if !frame.is_well_formed() {
            return Err(PresentError::MalformedFrame {
                width: frame.width,
                height: frame.height,
                bytes: frame.rgba.len(),
            });
        }
        let at = frame.position;
        self.pacer.show(frame);
        self.compose(Pace::Presented(at))
    }

    fn compose(&self, pace: Pace) -> Result<Presentation, PresentError> {
        let px = (self.surface.width as usize) * (self.surface.height as usize);
        let mut pixels = vec![0u8; px * 4];
        for p in pixels.chunks_exact_mut(4) {
            p[3] = 255;
        }

        let (quad, position) = match self.pacer.current() {
            Some(frame) if frame.is_well_formed() && frame.width > 0 && frame.height > 0 => {
                let quad = letterbox(frame.resolution(), self.surface);
                blit(frame, quad, self.surface, &mut pixels);
                (quad, Some(frame.position))
            }
            Some(frame) => {
                return Err(PresentError::MalformedFrame {
                    width: frame.width,
                    height: frame.height,
                    bytes: frame.rgba.len(),
                });
            }
            None => (
                Quad {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                None,
            ),
        };

        let overlay = if self.host.allows_overlay() {
            self.overlay_cfg.build(position, self.fps, quad)
        } else {
            Overlay::default()
        };

        Ok(Presentation {
            surface: self.surface,
            pixels,
            quad,
            position,
            overlay,
            pace,
        })
    }
}

/// Nearest-neighbour scale of `frame` into `quad`.
///
/// Nearest neighbour, not bilinear: the frame-accuracy tests assert on exact
/// pixel values, and a filter would blend two frames' signatures into a third
/// colour that belongs to neither.
fn blit(frame: &VideoFrame, quad: Quad, surface: Resolution, out: &mut [u8]) {
    if quad.width == 0 || quad.height == 0 {
        return;
    }
    for row in 0..quad.height {
        let sy = (u64::from(row) * u64::from(frame.height) / u64::from(quad.height))
            .min(u64::from(frame.height) - 1) as u32;
        for col in 0..quad.width {
            let sx = (u64::from(col) * u64::from(frame.width) / u64::from(quad.width))
                .min(u64::from(frame.width) - 1) as u32;
            let si = ((sy as usize) * (frame.width as usize) + (sx as usize)) * 4;
            let di = (((quad.y + row) as usize) * (surface.width as usize)
                + ((quad.x + col) as usize))
                * 4;
            let (Some(src), Some(dst)) = (frame.rgba.get(si..si + 4), out.get_mut(di..di + 4))
            else {
                continue;
            };
            dst.copy_from_slice(src);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use davimci_backend::MockBackend;
    use davimci_backend::mock::mock_signature;

    fn res(width: u32, height: u32) -> Resolution {
        Resolution { width, height }
    }

    #[test]
    fn a_presented_frame_fills_its_quad_and_leaves_black_bars() {
        let mut p = Presenter::new(Host::Embedded, res(20, 20), Fps::FPS_60);
        let frame = VideoFrame {
            position: Frame(3),
            width: 4,
            height: 2,
            rgba: [7u8, 8, 9, 255].repeat(8),
        };
        let out = p.present_frame(frame).unwrap();
        assert_eq!(
            out.quad,
            Quad {
                x: 0,
                y: 5,
                width: 20,
                height: 10
            }
        );
        assert_eq!(out.pixel(10, 10), [7, 8, 9, 255]);
        assert_eq!(out.pixel(10, 0), [0, 0, 0, 255], "bar is not black");
        assert_eq!(out.position, Some(Frame(3)));
    }

    #[test]
    fn hosts_produce_identical_video_pixels() {
        let frame = VideoFrame {
            position: Frame(9),
            width: 6,
            height: 4,
            rgba: (0..24u8).flat_map(|i| [i, i, i, 255]).collect(),
        };
        let mut embedded = Presenter::new(Host::Embedded, res(31, 17), Fps::FPS_60);
        let mut detached = Presenter::new(Host::Detached, res(31, 17), Fps::FPS_60);
        let a = embedded.present_frame(frame.clone()).unwrap();
        let b = detached.present_frame(frame).unwrap();
        assert_eq!(a.pixels, b.pixels, "the two host paths have diverged");
        assert_eq!(a.quad, b.quad);
        // The only permitted difference:
        assert!(a.overlay.timecode.is_some());
        assert_eq!(b.overlay, Overlay::default());
    }

    #[test]
    fn a_malformed_frame_is_refused_rather_than_read_past_its_end() {
        let mut p = Presenter::new(Host::Embedded, res(8, 8), Fps::FPS_60);
        let bad = VideoFrame {
            position: Frame(0),
            width: 100,
            height: 100,
            rgba: vec![0; 16],
        };
        assert!(matches!(
            p.present_frame(bad),
            Err(PresentError::MalformedFrame { .. })
        ));
    }

    #[test]
    fn nothing_presented_yet_composes_an_all_black_surface() {
        let mut p = Presenter::new(Host::Embedded, res(4, 4), Fps::FPS_60);
        let mut b = MockBackend::new(res(4, 4));
        let out = p.present(&mut b).unwrap();
        assert_eq!(out.pace, Pace::Empty);
        assert!(out.pixels.chunks_exact(4).all(|p| p == [0, 0, 0, 255]));
    }

    #[test]
    fn playback_presents_the_frame_the_clock_asked_for() {
        let mut b = MockBackend::new(res(4, 2));
        b.preview_start(Frame::ZERO, PreviewScale::Full).unwrap();
        let mut p = Presenter::new(Host::Embedded, res(8, 4), Fps::FPS_60);
        let out = p.present(&mut b).unwrap();
        assert_eq!(out.position, Some(Frame(0)));
        assert_eq!(out.pixel(4, 2), mock_signature(Frame(0)));
        assert_eq!(out.overlay.timecode.as_deref(), Some("00:00:00:00"));
    }

    #[test]
    fn auto_scale_never_decodes_below_what_is_drawn() {
        let source = res(1920, 1080);
        let mut p = Presenter::new(Host::Embedded, res(1920, 1080), Fps::FPS_60);
        p.auto_scale(source);
        assert_eq!(p.scale(), PreviewScale::Full);
        p.resize(res(900, 500));
        p.auto_scale(source);
        assert_eq!(p.scale(), PreviewScale::Half);
        p.resize(res(200, 100));
        p.auto_scale(source);
        assert_eq!(p.scale(), PreviewScale::Quarter);
    }

    #[test]
    fn a_zero_sized_surface_composes_nothing_and_does_not_panic() {
        let mut p = Presenter::new(Host::Embedded, res(0, 0), Fps::FPS_60);
        let out = p
            .present_frame(VideoFrame::black(Frame(0), res(4, 4)))
            .unwrap();
        assert!(out.pixels.is_empty());
        assert_eq!(out.quad.width, 0);
    }
}
