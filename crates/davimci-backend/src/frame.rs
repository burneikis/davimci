//! Frames handed from the backend to the presenter.

use davimci_core::{Frame, Resolution};

/// Preview decode scale.
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

/// The layout of a decoded frame's bytes.
///
/// RGBA8 is the default and the only format the golden-pixel, snapshot and
/// cross-frontend parity tests ever assert against. Planar is an opt-in a
/// host asks for when it can convert on the GPU: it halves the bytes that
/// cross to the card and moves colour conversion off the CPU, but it is
/// never the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PixelFormat {
    #[default]
    Rgba8,
    /// 8-bit YUV 4:2:0, three planes, chroma at half resolution on both
    /// axes.
    Yuv420p,
}

impl PixelFormat {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Rgba8 => "rgba8",
            Self::Yuv420p => "yuv420p",
        }
    }
}

/// One decoded frame in planar YUV 4:2:0.
///
/// Handed to a host that uploads three single-channel textures and converts
/// in a shader. [`PlanarFrame::to_rgba`] is the CPU reference for the same
/// picture, which is what the shader is asserted against and what a host
/// without a GPU path falls back to.
#[derive(Clone, PartialEq, Eq)]
pub struct PlanarFrame {
    pub position: Frame,
    pub width: u32,
    pub height: u32,
    /// Full-resolution luma, `width * height` bytes.
    pub y: Vec<u8>,
    /// Half-resolution chroma, `chroma_width * chroma_height` bytes each.
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl PlanarFrame {
    #[must_use]
    pub fn chroma_width(&self) -> u32 {
        self.width.div_ceil(2)
    }

    #[must_use]
    pub fn chroma_height(&self) -> u32 {
        self.height.div_ceil(2)
    }

    /// Whether every plane is exactly the length its dimensions imply.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        let luma = (self.width as usize) * (self.height as usize);
        let chroma = (self.chroma_width() as usize) * (self.chroma_height() as usize);
        self.y.len() == luma && self.u.len() == chroma && self.v.len() == chroma
    }

    /// Bytes a host would upload for this frame. Compared against
    /// `width * height * 4` by the benchmark that justifies this path.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.y.len() + self.u.len() + self.v.len()
    }

    /// The same picture as RGBA8, by the BT.709 limited-range matrix.
    ///
    /// The CPU reference for the shader: a host that converts on the GPU has
    /// to reproduce these pixels within a documented tolerance, and a host
    /// that cannot convert at all uses this.
    #[must_use]
    pub fn to_rgba(&self) -> VideoFrame {
        let mut rgba = vec![255u8; (self.width as usize) * (self.height as usize) * 4];
        let cw = self.chroma_width() as usize;
        for row in 0..self.height as usize {
            for col in 0..self.width as usize {
                let y = self
                    .y
                    .get(row * self.width as usize + col)
                    .copied()
                    .unwrap_or(16);
                let ci = (row / 2) * cw + col / 2;
                let u = self.u.get(ci).copied().unwrap_or(128);
                let v = self.v.get(ci).copied().unwrap_or(128);
                let px = yuv_to_rgb(y, u, v);
                let di = (row * self.width as usize + col) * 4;
                if let Some(dst) = rgba.get_mut(di..di + 3) {
                    dst.copy_from_slice(&px);
                }
            }
        }
        VideoFrame {
            position: self.position,
            width: self.width,
            height: self.height,
            rgba,
        }
    }
}

impl std::fmt::Debug for PlanarFrame {
    /// Never dump the planes, same rule as [`VideoFrame`].
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanarFrame")
            .field("position", &self.position)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.bytes())
            .finish_non_exhaustive()
    }
}

/// BT.709 limited range, integer arithmetic and no floats, so two machines
/// cannot disagree about a pixel.
#[must_use]
fn yuv_to_rgb(luma: u8, cb: u8, cr: u8) -> [u8; 3] {
    let luma = (i32::from(luma) - 16) * 1192;
    let cb = i32::from(cb) - 128;
    let cr = i32::from(cr) - 128;
    let red = (luma + 1836 * cr) >> 10;
    let green = (luma - 218 * cb - 546 * cr) >> 10;
    let blue = (luma + 2163 * cb) >> 10;
    [clamp_byte(red), clamp_byte(green), clamp_byte(blue)]
}

fn clamp_byte(v: i32) -> u8 {
    u8::try_from(v.clamp(0, 255)).unwrap_or(0)
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
    /// A black frame, used for the "degrade locally" decode policy.
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
        // Each sum is at most 255 * n, so the mean is always a byte.
        let mut out = [0u8; 4];
        for (o, s) in out.iter_mut().zip(sums) {
            *o = u8::try_from(s / n).unwrap_or(u8::MAX);
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

    fn planar(width: u32, height: u32, y: u8, u: u8, v: u8) -> PlanarFrame {
        let chroma = (width.div_ceil(2) as usize) * (height.div_ceil(2) as usize);
        PlanarFrame {
            position: Frame(0),
            width,
            height,
            y: vec![y; (width as usize) * (height as usize)],
            u: vec![u; chroma],
            v: vec![v; chroma],
        }
    }

    #[test]
    fn planar_black_and_white_convert_to_black_and_white() {
        // Limited range: 16 is black and 235 is white, which is exactly the
        // pair a full-range conversion would get wrong.
        let black = planar(4, 2, 16, 128, 128).to_rgba();
        assert_eq!(black.rgba[..4], [0, 0, 0, 255]);
        let white = planar(4, 2, 235, 128, 128).to_rgba();
        for c in 0..3 {
            assert!(white.rgba[c] >= 254, "white came out at {}", white.rgba[c]);
        }
        assert!(white.is_well_formed());
    }

    #[test]
    fn a_planar_frame_is_half_the_bytes_of_the_same_picture_in_rgba() {
        let frame = planar(1920, 1080, 128, 128, 128);
        assert!(frame.is_well_formed());
        let rgba = (frame.width as usize) * (frame.height as usize) * 4;
        // 1.5 bytes per pixel against 4: three eighths of the upload.
        assert_eq!(frame.bytes(), rgba * 3 / 8);
    }

    #[test]
    fn a_short_plane_is_not_well_formed_and_does_not_read_past_its_end() {
        let mut frame = planar(4, 2, 200, 90, 240);
        frame.u.pop();
        assert!(!frame.is_well_formed());
        // Conversion still produces a whole picture rather than panicking:
        // the missing chroma reads as neutral.
        assert!(frame.to_rgba().is_well_formed());
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
