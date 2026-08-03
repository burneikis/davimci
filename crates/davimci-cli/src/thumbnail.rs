//! Turning a decoded frame into a timeline thumbnail (idea.md, spec §15.2).
//!
//! Integral nearest-neighbour scaling, deliberately: a thumbnail is drawn a
//! few pixels tall, the arithmetic has to be exactly reproducible for the
//! rendering tests, and a filtered downscale would cost more than the decode
//! it came from.

use davimci_app::Thumbnail;
use davimci_backend::VideoFrame;
use davimci_core::Frame;

/// Scale `frame` down to `height` pixels, keeping its aspect ratio.
///
/// A frame smaller than the target is used as it is rather than magnified:
/// blowing up a quarter-res pull would look worse than a small picture.
#[must_use]
pub fn downscale(frame: &VideoFrame, height: u32, source_in: Frame) -> Thumbnail {
    let src_w = frame.width.max(1);
    let src_h = frame.height.max(1);
    let out_h = height.max(1).min(src_h);
    let out_w = (src_w * out_h / src_h).max(1);
    let mut rgba = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    for y in 0..out_h {
        let sy = y * src_h / out_h;
        for x in 0..out_w {
            let sx = x * src_w / out_w;
            let si = ((sy * src_w + sx) as usize) * 4;
            let di = ((y * out_w + x) as usize) * 4;
            if si + 4 <= frame.rgba.len() {
                rgba[di..di + 4].copy_from_slice(&frame.rgba[si..si + 4]);
            } else {
                // A short buffer is a decode that went wrong; an opaque
                // black pixel is a picture, not a panic.
                rgba[di + 3] = 255;
            }
        }
    }
    Thumbnail::new(out_w, out_h, rgba, source_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32, fill: u8) -> VideoFrame {
        VideoFrame {
            position: Frame(0),
            width,
            height,
            rgba: vec![fill; (width as usize) * (height as usize) * 4],
        }
    }

    #[test]
    fn a_downscale_keeps_the_aspect_ratio_and_is_well_formed() {
        let t = downscale(&frame(320, 180, 200), 36, Frame(7));
        assert_eq!((t.width, t.height), (64, 36));
        assert!(t.is_well_formed());
        assert_eq!(t.source_in, Frame(7));
        assert!(t.rgba.iter().all(|b| *b == 200));
    }

    #[test]
    fn a_frame_smaller_than_the_target_is_not_magnified() {
        let t = downscale(&frame(16, 9, 10), 64, Frame(0));
        assert_eq!((t.width, t.height), (16, 9));
    }

    #[test]
    fn a_short_buffer_produces_a_picture_rather_than_a_panic() {
        let mut f = frame(8, 8, 5);
        f.rgba.truncate(4);
        let t = downscale(&f, 4, Frame(0));
        assert!(t.is_well_formed());
    }
}
