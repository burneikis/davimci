//! Aspect fitting.
//!
//! A video quad is letterboxed into its surface: never stretched, never
//! cropped, always centred, and always integral - a half-pixel quad would
//! make the image-diff tests ambiguous and would shimmer while resizing.

use davimci_core::Resolution;

/// Destination rectangle, in surface pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quad {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Quad {
    #[must_use]
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// Largest centred rectangle inside `surface` with `source`'s aspect ratio.
///
/// Degenerate inputs (a zero-sized surface or source) give a zero-sized quad
/// rather than a division by zero: a frontend mid-resize must not crash.
#[must_use]
pub fn letterbox(source: Resolution, surface: Resolution) -> Quad {
    if source.width == 0 || source.height == 0 || surface.width == 0 || surface.height == 0 {
        return Quad {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
    }
    let sw = u64::from(source.width);
    let sh = u64::from(source.height);
    let tw = u64::from(surface.width);
    let th = u64::from(surface.height);

    // Compare aspects by cross-multiplication: no floats, so the same inputs
    // give the same quad on every machine.
    let (width, height) = if sw * th >= tw * sh {
        // Source is wider: pin to surface width.
        let h = (tw * sh / sw).max(1);
        (tw, h)
    } else {
        let w = (th * sw / sh).max(1);
        (w, th)
    };
    let width = u32::try_from(width).unwrap_or(surface.width);
    let height = u32::try_from(height).unwrap_or(surface.height);
    Quad {
        x: (surface.width.saturating_sub(width)) / 2,
        y: (surface.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(width: u32, height: u32) -> Resolution {
        Resolution { width, height }
    }

    #[test]
    fn equal_aspect_fills_the_surface() {
        let q = letterbox(res(1920, 1080), res(640, 360));
        assert_eq!(
            q,
            Quad {
                x: 0,
                y: 0,
                width: 640,
                height: 360
            }
        );
    }

    #[test]
    fn wide_source_gets_horizontal_bars() {
        let q = letterbox(res(1920, 800), res(800, 800));
        assert_eq!(q.width, 800);
        assert_eq!(q.height, 333);
        assert_eq!(q.x, 0);
        assert_eq!(q.y, (800 - 333) / 2);
    }

    #[test]
    fn tall_source_gets_vertical_bars() {
        let q = letterbox(res(1080, 1920), res(800, 800));
        assert_eq!(q.height, 800);
        assert_eq!(q.width, 450);
        assert_eq!(q.y, 0);
    }

    #[test]
    fn aspect_matrix_never_exceeds_the_surface_or_collapses() {
        let surfaces = [res(320, 240), res(1920, 1080), res(1, 1), res(7, 13)];
        let sources = [
            res(1920, 1080),
            res(720, 576),
            res(1080, 1920),
            res(4096, 1716),
        ];
        for s in surfaces {
            for src in sources {
                let q = letterbox(src, s);
                assert!(q.width <= s.width && q.height <= s.height, "{q:?} in {s:?}");
                assert!(q.width > 0 && q.height > 0, "{q:?}");
                assert!(q.x + q.width <= s.width);
                assert!(q.y + q.height <= s.height);
            }
        }
    }

    #[test]
    fn degenerate_input_gives_an_empty_quad() {
        assert_eq!(letterbox(res(0, 0), res(10, 10)).width, 0);
        assert_eq!(letterbox(res(10, 10), res(0, 5)).height, 0);
    }
}
