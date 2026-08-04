//! Inline preview: a composited frame downsampled into terminal rows.
//!
//! The picture the window uploads as a texture is the picture drawn here, so
//! nothing in this module decides anything about the frame - it arrives from
//! `davimci-present` already paced and letterboxed, and all that happens is a
//! resample into whatever cells `:set previewheight` asked for and an encode
//! into the one protocol detection settled on at startup.
//!
//! Escape-sequence throughput guarantees no pacing, so encoding runs off the
//! event loop in [`Encoder`]: a submitted frame that is still being encoded
//! when a newer one arrives is dropped, never queued, and the loop never
//! waits for either.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use davimci_core::Resolution;
use davimci_present::{Presentation, letterbox};
use ratatui::prelude::{Line, Span, Style};
use ratatui::style::Color;

/// How the terminal is asked to draw a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Kitty graphics: RGB, base64, chunked, placed at the cursor.
    Kitty,
    /// Sixel, quantised to the 6x6x6 colour cube.
    Sixel,
    /// Truecolour half-blocks - the floor, and the only one that needs no
    /// capability at all beyond 24-bit colour.
    Blocks,
}

impl Protocol {
    /// A `:set previewprotocol` value. `auto` is not one of these; the caller
    /// resolves that with [`detect`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "kitty" => Some(Self::Kitty),
            "sixel" => Some(Self::Sixel),
            "blocks" => Some(Self::Blocks),
            _ => None,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Blocks => "blocks",
        }
    }

    /// Pixels one cell holds. Half-blocks are one column by two rows by
    /// construction; the graphics protocols need the terminal's real cell,
    /// which only the terminal can measure.
    #[must_use]
    fn cell(self, measured: Cell) -> Cell {
        match self {
            Self::Blocks => Cell {
                width: 1,
                height: 2,
            },
            _ => measured,
        }
    }
}

/// A terminal cell in pixels, as reported by the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub width: u16,
    pub height: u16,
}

impl Default for Cell {
    /// What a terminal that will not answer is assumed to have. Wrong by a
    /// little only skews the aspect of a graphics preview; it cannot break
    /// the layout, which is counted in cells.
    fn default() -> Self {
        Self {
            width: 10,
            height: 20,
        }
    }
}

/// The band the preview is being drawn into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub columns: u16,
    pub rows: u16,
    pub protocol: Protocol,
    pub cell: Cell,
}

impl Layout {
    /// The pixel box these cells cover.
    #[must_use]
    pub fn pixels(&self) -> Resolution {
        let cell = self.protocol.cell(self.cell);
        Resolution {
            width: u32::from(self.columns) * u32::from(cell.width),
            height: u32::from(self.rows) * u32::from(cell.height),
        }
    }
}

/// One encoded preview, ready to go on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Band {
    /// Rows it occupies, whichever protocol drew it.
    pub rows: u16,
    /// Character cells, for the half-block encoder.
    pub cells: Vec<Line<'static>>,
    /// Raw bytes for a graphics protocol, written at the top-left of the band
    /// after the rows are drawn.
    pub escape: Option<Vec<u8>>,
    /// Which frame this is, so an unchanged picture is not re-encoded.
    pub pixels_id: u64,
}

impl Band {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }
}

/// An RGB image, tightly packed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

impl Image {
    #[must_use]
    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y as usize) * (self.width as usize) + (x as usize)) * 3;
        match self.rgb.get(i..i + 3) {
            Some(px) => [px[0], px[1], px[2]],
            None => [0, 0, 0],
        }
    }
}

/// Which protocol this terminal gets, absent an override.
///
/// Queried from the environment once and never per frame: capability queries
/// are unreliable through multiplexers, which is why `:set previewprotocol`
/// exists at all. Anything unrecognised gets half-blocks, so the feature
/// never depends on a query succeeding.
#[must_use]
pub fn detect() -> Protocol {
    let var = |k: &str| std::env::var(k).unwrap_or_default().to_lowercase();
    let term = var("TERM");
    let program = var("TERM_PROGRAM");
    if !var("KITTY_WINDOW_ID").is_empty()
        || term.contains("kitty")
        || program.contains("kitty")
        || program.contains("ghostty")
        || program.contains("wezterm")
    {
        return Protocol::Kitty;
    }
    if term.contains("sixel") || term.contains("foot") || term.contains("mlterm") {
        return Protocol::Sixel;
    }
    Protocol::Blocks
}

/// Resample `pres` into `target`, letterboxed - never stretched, never
/// cropped, the same rule `davimci-present` follows for its own surface.
#[must_use]
pub fn downsample(pres: &Presentation, target: Resolution) -> Image {
    let mut rgb = vec![0u8; (target.width as usize) * (target.height as usize) * 3];
    let quad = letterbox(pres.surface, target);
    if quad.width == 0 || quad.height == 0 {
        return Image {
            width: target.width,
            height: target.height,
            rgb,
        };
    }
    for y in 0..quad.height {
        // Nearest neighbour: a band a few dozen cells tall is a heavy
        // decimation, and a box filter would cost the whole source frame per
        // preview row for a difference no cell can show.
        let sy = (u64::from(y) * u64::from(pres.surface.height) / u64::from(quad.height)) as u32;
        for x in 0..quad.width {
            let sx = (u64::from(x) * u64::from(pres.surface.width) / u64::from(quad.width)) as u32;
            let px = pres.pixel(sx, sy);
            let i =
                (((y + quad.y) as usize) * (target.width as usize) + ((x + quad.x) as usize)) * 3;
            if let Some(out) = rgb.get_mut(i..i + 3) {
                out.copy_from_slice(&px[..3]);
            }
        }
    }
    Image {
        width: target.width,
        height: target.height,
        rgb,
    }
}

/// Downsample and encode in one step - what a host calls per frame.
#[must_use]
pub fn paint(pres: &Presentation, layout: Layout) -> Band {
    if layout.rows == 0 || layout.columns == 0 {
        return Band::default();
    }
    let image = downsample(pres, layout.pixels());
    let mut band = encode(&image, layout);
    band.pixels_id = pres.pixels_id;
    band
}

/// Encode an already-fitted image for `layout`'s protocol.
#[must_use]
pub fn encode(image: &Image, layout: Layout) -> Band {
    match layout.protocol {
        Protocol::Blocks => Band {
            rows: layout.rows,
            cells: half_blocks(image, layout),
            escape: None,
            pixels_id: 0,
        },
        Protocol::Kitty => Band {
            rows: layout.rows,
            cells: Vec::new(),
            escape: Some(kitty(image, layout)),
            pixels_id: 0,
        },
        Protocol::Sixel => Band {
            rows: layout.rows,
            cells: Vec::new(),
            escape: Some(sixel(image)),
            pixels_id: 0,
        },
    }
}

/// Two pixel rows per character row: the top half is the foreground of
/// `▀`, the bottom half its background.
fn half_blocks(image: &Image, layout: Layout) -> Vec<Line<'static>> {
    let mut out = Vec::with_capacity(usize::from(layout.rows));
    for row in 0..u32::from(layout.rows) {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut run = String::new();
        let mut run_style = Style::default();
        for column in 0..u32::from(layout.columns) {
            let top = image.pixel(column, row * 2);
            let bottom = image.pixel(column, row * 2 + 1);
            let style = Style::default()
                .fg(Color::Rgb(top[0], top[1], top[2]))
                .bg(Color::Rgb(bottom[0], bottom[1], bottom[2]));
            if style != run_style && !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), run_style));
            }
            run_style = style;
            run.push('\u{2580}');
        }
        if !run.is_empty() {
            spans.push(Span::styled(run, run_style));
        }
        out.push(Line::from(spans));
    }
    out
}

/// Base64 payload chars per escape chunk, as the kitty protocol asks.
const KITTY_CHUNK: usize = 4096;

/// Kitty graphics: delete what was there, then transmit-and-display RGB.
fn kitty(image: &Image, layout: Layout) -> Vec<u8> {
    let mut out = b"\x1b_Ga=d,d=A\x1b\\".to_vec();
    let payload = base64(&image.rgb);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(KITTY_CHUNK).collect();
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        let header = if i == 0 {
            format!(
                "\x1b_Ga=T,f=24,s={},v={},c={},r={},q=2,m={more};",
                image.width, image.height, layout.columns, layout.rows
            )
        } else {
            format!("\x1b_Gm={more};")
        };
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\x1b\\");
    }
    out
}

/// Sixel, quantised to the 6x6x6 cube: 216 registers is a fixed palette, so
/// encoding cost does not depend on the picture.
fn sixel(image: &Image) -> Vec<u8> {
    let mut out = format!("\x1bPq\"1;1;{};{}", image.width, image.height).into_bytes();
    let mut declared = [false; 216];
    for band in 0..image.height.div_ceil(6) {
        // One pass per colour present in the band, which is what the format
        // requires: a register is selected, then its whole row written.
        let mut used = [false; 216];
        let mut mask: Vec<Vec<u8>> = vec![Vec::new(); 216];
        for x in 0..image.width {
            for dy in 0..6u32 {
                let y = band * 6 + dy;
                if y >= image.height {
                    continue;
                }
                let index = cube_index(image.pixel(x, y));
                used[index] = true;
                let row = &mut mask[index];
                if row.len() <= x as usize {
                    row.resize(x as usize + 1, 0);
                }
                row[x as usize] |= 1 << dy;
            }
        }
        let mut first = true;
        for (index, present) in used.iter().enumerate() {
            if !*present {
                continue;
            }
            if !first {
                out.push(b'$');
            }
            first = false;
            if !declared[index] {
                declared[index] = true;
                let (r, g, b) = cube_colour(index);
                out.extend_from_slice(format!("#{index};2;{r};{g};{b}").as_bytes());
            } else {
                out.extend_from_slice(format!("#{index}").as_bytes());
            }
            for x in 0..image.width as usize {
                let bits = mask[index].get(x).copied().unwrap_or(0);
                out.push(b'?' + bits);
            }
        }
        out.push(b'-');
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Index into the 6x6x6 colour cube.
fn cube_index(px: [u8; 3]) -> usize {
    let q = |v: u8| (u32::from(v) * 5 / 255) as usize;
    q(px[0]) * 36 + q(px[1]) * 6 + q(px[2])
}

/// The cube entry as sixel's percentage components.
fn cube_colour(index: usize) -> (u32, u32, u32) {
    let step = |v: usize| (v as u32) * 100 / 5;
    (step(index / 36), step((index / 6) % 6), step(index % 6))
}

/// Standard base64, no padding omitted, no line breaks.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let c = |shift: u32| char::from(ALPHABET[((n >> shift) & 0x3f) as usize]);
        out.push(c(18));
        out.push(c(12));
        out.push(if chunk.len() > 1 { c(6) } else { '=' });
        out.push(if chunk.len() > 2 { c(0) } else { '=' });
    }
    out
}

type EncodeFn = Box<dyn Fn(&Presentation, Layout) -> Band + Send + Sync>;

#[derive(Default)]
struct Slot {
    job: Option<(Presentation, Layout)>,
    done: Option<Band>,
    stop: bool,
}

/// Encoding off the event loop.
///
/// The slot holds one job: submitting while an older frame is still waiting
/// replaces it, so a terminal that cannot keep up loses pictures rather than
/// accumulating a backlog behind the audio clock.
pub struct Encoder {
    slot: Arc<(Mutex<Slot>, Condvar)>,
    busy: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    submitted: Option<(u64, Layout)>,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("submitted", &self.submitted)
            .finish_non_exhaustive()
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    #[must_use]
    pub fn new() -> Self {
        Self::with_encoder(paint)
    }

    /// An encoder with the encode step replaced - what the throughput test
    /// makes artificially slow.
    #[must_use]
    pub fn with_encoder<F>(encode: F) -> Self
    where
        F: Fn(&Presentation, Layout) -> Band + Send + Sync + 'static,
    {
        let encode: EncodeFn = Box::new(encode);
        let slot = Arc::new((Mutex::new(Slot::default()), Condvar::new()));
        let busy = Arc::new(AtomicBool::new(false));
        let worker = Arc::clone(&slot);
        let worker_busy = Arc::clone(&busy);
        let thread = std::thread::Builder::new()
            .name("davimci-preview".into())
            .spawn(move || {
                let (lock, cv) = &*worker;
                loop {
                    let job = {
                        let Ok(mut guard) = lock.lock() else { return };
                        loop {
                            if guard.stop {
                                return;
                            }
                            if let Some(job) = guard.job.take() {
                                break job;
                            }
                            let Ok(next) = cv.wait(guard) else { return };
                            guard = next;
                        }
                    };
                    worker_busy.store(true, Ordering::SeqCst);
                    let band = encode(&job.0, job.1);
                    worker_busy.store(false, Ordering::SeqCst);
                    let Ok(mut guard) = lock.lock() else { return };
                    guard.done = Some(band);
                }
            })
            .ok();
        Self {
            slot,
            busy,
            thread,
            submitted: None,
        }
    }

    /// Hand over a frame. Never blocks, and never waits for the last one.
    pub fn submit(&mut self, pres: &Presentation, layout: Layout) {
        if layout.rows == 0 {
            return;
        }
        // The same picture in the same band encodes to the same bytes, so a
        // repeated frame costs nothing.
        if self.submitted == Some((pres.pixels_id, layout)) {
            return;
        }
        let (lock, cv) = &*self.slot;
        let Ok(mut guard) = lock.lock() else { return };
        self.submitted = Some((pres.pixels_id, layout));
        guard.job = Some((pres.clone(), layout));
        cv.notify_one();
    }

    /// The newest finished band, or `None` when nothing new is ready - in
    /// which case the caller keeps showing what it had.
    pub fn take(&mut self) -> Option<Band> {
        let (lock, _) = &*self.slot;
        lock.lock().ok()?.done.take()
    }

    /// Whether a frame is being encoded right now. Diagnostics and tests
    /// only; the loop never waits on it.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let (lock, cv) = &*self.slot;
        if let Ok(mut guard) = lock.lock() {
            guard.stop = true;
        }
        cv.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presentation(width: u32, height: u32, fill: [u8; 4]) -> Presentation {
        let surface = Resolution { width, height };
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&fill);
        }
        Presentation {
            surface,
            pixels: Arc::new(pixels),
            pixels_id: 1,
            quad: letterbox(surface, surface),
            position: None,
            overlay: Default::default(),
            pace: davimci_present::Pace::Empty,
        }
    }

    fn layout(protocol: Protocol, columns: u16, rows: u16) -> Layout {
        Layout {
            columns,
            rows,
            protocol,
            cell: Cell::default(),
        }
    }

    #[test]
    fn a_wide_frame_letterboxes_into_the_band_rather_than_stretching() {
        let pres = presentation(1920, 1080, [255, 255, 255, 255]);
        let l = layout(Protocol::Blocks, 40, 12);
        let image = downsample(&pres, l.pixels());
        assert_eq!((image.width, image.height), (40, 24));
        // 16:9 into 40x24 pins the width and leaves bars top and bottom.
        let quad = letterbox(pres.surface, l.pixels());
        assert_eq!((quad.width, quad.height), (40, 22));
        assert_eq!(quad.y, 1);
        assert_eq!(image.pixel(0, 0), [0, 0, 0], "no bar at the top");
        assert_eq!(image.pixel(0, 12), [255, 255, 255]);
        assert_eq!(image.pixel(0, 23), [0, 0, 0], "no bar at the bottom");
    }

    #[test]
    fn half_blocks_encode_two_pixel_rows_per_cell() {
        let image = Image {
            width: 2,
            height: 2,
            rgb: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };
        let lines = half_blocks(&image, layout(Protocol::Blocks, 2, 1));
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        assert_eq!(spans.len(), 2, "two colours means two spans");
        assert_eq!(spans[0].content.as_ref(), "\u{2580}");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(spans[0].style.bg, Some(Color::Rgb(7, 8, 9)));
        assert_eq!(spans[1].style.fg, Some(Color::Rgb(4, 5, 6)));
        assert_eq!(spans[1].style.bg, Some(Color::Rgb(10, 11, 12)));
    }

    #[test]
    fn kitty_transmits_rgb_at_the_cursor() {
        let image = Image {
            width: 2,
            height: 1,
            rgb: vec![255, 0, 0, 0, 0, 255],
        };
        let bytes = kitty(&image, layout(Protocol::Kitty, 2, 1));
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "\x1b_Ga=d,d=A\x1b\\\x1b_Ga=T,f=24,s=2,v=1,c=2,r=1,q=2,m=0;/wAAAAD/\x1b\\"
        );
    }

    #[test]
    fn a_long_kitty_payload_is_chunked() {
        let image = Image {
            width: 4096,
            height: 1,
            rgb: vec![0; 4096 * 3],
        };
        let bytes = kitty(&image, layout(Protocol::Kitty, 8, 1));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("m=1;"), "first chunk must ask for more");
        assert!(text.ends_with("\x1b\\"));
        assert_eq!(
            text.matches("\x1b_G").count(),
            1 + 4,
            "delete plus 4 chunks"
        );
        assert!(text.contains("\x1b_Gm=0;"), "last chunk must close");
    }

    #[test]
    fn sixel_writes_one_band_per_six_pixel_rows() {
        let image = Image {
            width: 1,
            height: 2,
            rgb: vec![255, 0, 0, 255, 0, 0],
        };
        let bytes = sixel(&image);
        // Red is cube entry 5*36 = 180; both rows set, so bits 0b11 -> '?'+3.
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "\x1bPq\"1;1;1;2#180;2;100;0;0B-\x1b\\"
        );
    }

    #[test]
    fn base64_pads_a_short_tail() {
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b"abc"), "YWJj");
    }

    #[test]
    fn a_slow_encoder_never_blocks_the_caller() {
        let mut enc = Encoder::with_encoder(|_, l| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            Band {
                rows: l.rows,
                ..Band::default()
            }
        });
        let start = std::time::Instant::now();
        let mut iterations = 0;
        // A loop that submits every "tick" must keep spinning while the
        // encoder is still on the first frame.
        while start.elapsed() < std::time::Duration::from_millis(60) {
            let mut pres = presentation(16, 16, [1, 2, 3, 255]);
            pres.pixels_id = iterations;
            enc.submit(&pres, layout(Protocol::Blocks, 8, 4));
            let _ = enc.take();
            iterations += 1;
        }
        assert!(
            iterations > 100,
            "the loop stalled on the encoder ({iterations} iterations)"
        );
    }

    #[test]
    fn frames_are_dropped_rather_than_queued() {
        let mut enc = Encoder::with_encoder(|p, l| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            Band {
                rows: l.rows,
                pixels_id: p.pixels_id,
                ..Band::default()
            }
        });
        for id in 0..5 {
            let mut pres = presentation(16, 16, [0, 0, 0, 255]);
            pres.pixels_id = id;
            enc.submit(&pres, layout(Protocol::Blocks, 8, 4));
        }
        let mut seen = Vec::new();
        let start = std::time::Instant::now();
        while start.elapsed() < std::time::Duration::from_millis(300) {
            if let Some(band) = enc.take() {
                seen.push(band.pixels_id);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            seen.len() < 5,
            "every submitted frame was encoded: {seen:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&4),
            "the newest frame must be the one kept"
        );
    }

    #[test]
    fn a_repeated_frame_is_not_re_encoded() {
        let mut enc = Encoder::new();
        let pres = presentation(16, 16, [9, 9, 9, 255]);
        let l = layout(Protocol::Blocks, 8, 4);
        enc.submit(&pres, l);
        while enc.take().is_none() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        enc.submit(&pres, l);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(enc.take().is_none());
    }

    #[test]
    fn the_detected_protocol_is_overridable_by_name() {
        assert_eq!(Protocol::parse("kitty"), Some(Protocol::Kitty));
        assert_eq!(Protocol::parse("sixel"), Some(Protocol::Sixel));
        assert_eq!(Protocol::parse("blocks"), Some(Protocol::Blocks));
        assert_eq!(Protocol::parse("auto"), None);
    }

    #[test]
    fn a_zero_row_band_encodes_nothing() {
        let pres = presentation(16, 16, [0, 0, 0, 255]);
        assert!(paint(&pres, layout(Protocol::Kitty, 40, 0)).is_empty());
        assert!(paint(&pres, layout(Protocol::Blocks, 0, 4)).is_empty());
    }
}
