//! Frames handed from the backend to the presenter.

use davimci_core::{Frame, Resolution};

/// Preview decode scale (plan.md Phase 6).
///
/// Scrubbing drops resolution rather than frames, and the TUI's small window
/// is cheap by construction: the request goes all the way down to
/// `mlt_frame_get_image()`, so a quarter-res pull decodes at quarter res.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewScale {
    #[default]
    Full,
    Half,
    Quarter,
}

impl PreviewScale {
    /// Divisor applied to both axes.
    #[must_use]
    pub fn divisor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }

    /// Scaled resolution, rounded to even dimensions and never zero.
    ///
    /// Even dimensions keep chroma-subsampled formats representable, so a
    /// scaled pull cannot land on a size a codec would have to round for us.
    #[must_use]
    pub fn apply(self, res: Resolution) -> Resolution {
        let d = self.divisor();
        let round = |v: u32| ((v / d).max(2)) & !1;
        Resolution {
            width: round(res.width),
            height: round(res.height),
        }
    }
}

/// One decoded video frame, RGBA8, tightly packed.
#[derive(Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Timeline position this frame presents.
    pub position: Frame,
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA8.
    pub rgba: Vec<u8>,
}

impl VideoFrame {
    /// A black frame, used for the Phase 0 "degrade locally" decode policy.
    #[must_use]
    pub fn black(position: Frame, res: Resolution) -> Self {
        let mut rgba = vec![0u8; (res.width as usize) * (res.height as usize) * 4];
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
        Self {
            position,
            width: res.width,
            height: res.height,
            rgba,
        }
    }

    #[must_use]
    pub fn resolution(&self) -> Resolution {
        Resolution {
            width: self.width,
            height: self.height,
        }
    }

    /// Whether the buffer length matches the declared dimensions.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.rgba.len() == (self.width as usize) * (self.height as usize) * 4
    }

    /// Average RGBA of the frame - the "pixel signature" the frame-accuracy
    /// tests compare, so a test never has to diff a whole buffer.
    #[must_use]
    pub fn signature(&self) -> [u8; 4] {
        if self.rgba.is_empty() {
            return [0; 4];
        }
        let mut sums = [0u64; 4];
        for px in self.rgba.chunks_exact(4) {
            for (s, v) in sums.iter_mut().zip(px) {
                *s += u64::from(*v);
            }
        }
        let n = (self.rgba.len() / 4) as u64;
        let mut out = [0u8; 4];
        for (o, s) in out.iter_mut().zip(sums) {
            *o = (s / n) as u8;
        }
        out
    }
}

impl std::fmt::Debug for VideoFrame {
    /// Never dump the pixel buffer: a frame in a log line must stay readable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoFrame")
            .field("position", &self.position)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn scale_halves_and_quarters_evenly() {
        let hd = Resolution::HD_1080;
        assert_eq!(PreviewScale::Full.apply(hd), hd);
        assert_eq!(
            PreviewScale::Half.apply(hd),
            Resolution {
                width: 960,
                height: 540
            }
        );
        assert_eq!(
            PreviewScale::Quarter.apply(hd),
            Resolution {
                width: 480,
                height: 270
            }
        );
    }

    #[test]
    fn scale_never_yields_zero_or_odd_dimensions() {
        let tiny = Resolution {
            width: 3,
            height: 1,
        };
        let out = PreviewScale::Quarter.apply(tiny);
        assert_eq!(out.width % 2, 0);
        assert_eq!(out.height % 2, 0);
        assert!(out.width >= 2 && out.height >= 2);
    }

    #[test]
    fn black_frame_is_opaque_and_well_formed() {
        let f = VideoFrame::black(
            Frame(7),
            Resolution {
                width: 4,
                height: 2,
            },
        );
        assert!(f.is_well_formed());
        assert_eq!(f.signature(), [0, 0, 0, 255]);
    }
}
