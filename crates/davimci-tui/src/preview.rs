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

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "terminal cell and pixel indices are bounded by the surface being encoded"
)]

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

/// What `:set previewheight` asked for, before the screen has its say.
///
/// Only this crate can turn one into rows: a percentage needs the screen, and
/// `Auto` needs the picture's aspect and the terminal's cell as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Height {
    #[default]
    Off,
    Rows(u16),
    Percent(u8),
    Auto,
}

/// Percentage of the screen the preview band may never exceed, so the
/// timeline always keeps rows of its own.
pub const MAX_SCREEN_PERCENT: u16 = 75;

impl Height {
    /// Rows the band gets on a screen of `screen` rows, where `natural` is
    /// what the picture could fill at the current width.
    ///
    /// `MAX_SCREEN_PERCENT` of the screen is the ceiling for all of them.
    #[must_use]
    pub fn rows(self, screen: u16, natural: u16) -> u16 {
        let cap = (u32::from(screen) * u32::from(MAX_SCREEN_PERCENT) / 100) as u16;
        match self {
            Self::Off => 0,
            Self::Rows(rows) => rows.min(cap),
            Self::Percent(pc) => ((u32::from(screen) * u32::from(pc) / 100) as u16).min(cap),
            Self::Auto => natural.min(cap),
        }
    }
}

/// Rows a picture of `aspect` fills across `columns` cells.
///
/// This is the height beyond which more rows buy nothing: the picture is
/// letterboxed into the band, so once it is as wide as the terminal, every
/// further row is blank. `:set previewheight auto` asks for exactly this,
/// which is why a wider terminal gives a taller band.
#[must_use]
pub fn natural_rows(columns: u16, protocol: Protocol, cell: Cell, aspect: Resolution) -> u16 {
    let cell = protocol.cell(cell);
    if aspect.width == 0 || cell.height == 0 {
        return 0;
    }
    let width = u64::from(columns) * u64::from(cell.width);
    let height = width * u64::from(aspect.height) / u64::from(aspect.width);
    let rows = height.div_ceil(u64::from(cell.height));
    u16::try_from(rows).unwrap_or(u16::MAX)
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

/// Sixel, with a palette chosen per frame.
///
/// A fixed colour cube bands badly: video occupies a narrow part of the gamut,
/// so most of the cube goes unused while the entries that are used sit a fifth
/// of the range from the pixel they stand for. Median cut over the frame's own
/// histogram spends all 256 registers where the picture actually is, and costs
/// one extra pass on the encoder thread.
fn sixel(image: &Image) -> Vec<u8> {
    let (palette, index_of) = quantise(image);
    let mut out = format!("\x1bPq\"1;1;{};{}", image.width, image.height).into_bytes();
    for (i, [r, g, b]) in palette.iter().enumerate() {
        // Sixel components are percentages, so rounding matters: truncating
        // here is a systematic darkening of the whole picture.
        let pc = |v: u8| (u32::from(v) * 100 + 127) / 255;
        out.extend_from_slice(format!("#{i};2;{};{};{}", pc(*r), pc(*g), pc(*b)).as_bytes());
    }

    let width = image.width as usize;
    // One row of bits per palette entry, reused band to band: the format wants
    // a register selected and then its whole row written.
    let mut mask = vec![0u8; width * palette.len()];
    for band in 0..image.height.div_ceil(6) {
        mask.fill(0);
        let mut used = vec![false; palette.len()];
        for x in 0..image.width {
            for dy in 0..6u32 {
                let y = band * 6 + dy;
                if y >= image.height {
                    continue;
                }
                let index = usize::from(index_of[bucket(image.pixel(x, y))]);
                used[index] = true;
                mask[index * width + x as usize] |= 1 << dy;
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
            out.extend_from_slice(format!("#{index}").as_bytes());
            let row = &mask[index * width..(index + 1) * width];
            // Trailing empties say nothing: a colour that stops halfway across
            // just ends its row there.
            let end = row.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
            run_length(&row[..end], &mut out);
        }
        out.push(b'-');
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Sixel's own repeat form, `!<count><char>`.
///
/// Preview payloads are megabytes of mostly flat picture, and the pty is the
/// narrowest part of the path: a run of one character costs three bytes plus
/// its digits, so anything longer than four is worth collapsing.
fn run_length(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < row.len() {
        let bits = row[i];
        let mut run = 1;
        while i + run < row.len() && row[i + run] == bits {
            run += 1;
        }
        let ch = b'?' + bits;
        if run > 4 {
            out.extend_from_slice(format!("!{run}").as_bytes());
            out.push(ch);
        } else {
            out.extend(std::iter::repeat_n(ch, run));
        }
        i += run;
    }
}

/// A 6-bit-per-channel histogram bin. Binning first keeps the median cut over
/// the bins rather than the pixels, so its cost depends on the number of
/// distinct colours and not on the size of the band.
///
/// Six bits, because the bin is the floor on accuracy: a bin four levels wide
/// matches sixel's own colour precision, which is a percentage per channel and
/// so ~2.5 levels. Fewer bits banded a gradient the palette could have
/// resolved; more would cost a megabyte of histogram per frame for accuracy
/// the format cannot carry.
const BINS: usize = 64 * 64 * 64;

/// Sixel allows 256 colour registers, and there is no reason to use fewer.
const MAX_COLOURS: usize = 256;

fn bucket(px: [u8; 3]) -> usize {
    let q = |v: u8| usize::from(v >> 2);
    (q(px[0]) << 12) | (q(px[1]) << 6) | q(px[2])
}

/// One occupied histogram bin: how many pixels fell in it and their totals, so
/// a box of bins can report the average colour of the pixels inside it rather
/// than the centre of the box.
#[derive(Clone, Copy)]
struct Bin {
    bucket: u32,
    count: u32,
    sum: [u32; 3],
}

/// The frame's palette, and the bin-to-entry map that assigns every pixel to
/// it.
///
/// Median cut partitions the bins, so each bin belongs to exactly one entry by
/// construction - there is no nearest-colour search per pixel, which is what
/// would make an adaptive palette too slow for preview.
fn quantise(image: &Image) -> (Vec<[u8; 3]>, Vec<u8>) {
    let mut bins: Vec<Bin> = Vec::new();
    let mut at = vec![u32::MAX; BINS];
    for y in 0..image.height {
        for x in 0..image.width {
            let px = image.pixel(x, y);
            let b = bucket(px);
            let slot = at[b];
            if slot == u32::MAX {
                at[b] = bins.len() as u32;
                bins.push(Bin {
                    bucket: b as u32,
                    count: 1,
                    sum: [u32::from(px[0]), u32::from(px[1]), u32::from(px[2])],
                });
            } else if let Some(bin) = bins.get_mut(slot as usize) {
                bin.count += 1;
                for (total, v) in bin.sum.iter_mut().zip(px) {
                    *total += u32::from(v);
                }
            }
        }
    }

    let mut boxes = vec![(0usize, bins.len())];
    while boxes.len() < MAX_COLOURS {
        // Split the box that costs the most: pixels inside it times how far
        // its widest channel spreads. Weighting by pixel count is what keeps
        // registers off a handful of stray highlights.
        let pick = boxes
            .iter()
            .enumerate()
            .filter(|(_, (s, e))| e - s > 1)
            .max_by_key(|(_, (s, e))| cost(&bins[*s..*e]))
            .map(|(i, _)| i);
        let Some(pick) = pick else { break };
        let (start, end) = boxes[pick];
        let axis = widest_axis(&bins[start..end]);
        bins[start..end].sort_unstable_by_key(|b| channel(b, axis));
        // The weighted median, so both halves carry a similar share of the
        // pixels rather than a similar share of the colours.
        let total: u64 = bins[start..end].iter().map(|b| u64::from(b.count)).sum();
        let mut acc = 0u64;
        let mut mid = start + 1;
        for (i, bin) in bins[start..end].iter().enumerate() {
            acc += u64::from(bin.count);
            if acc * 2 >= total {
                mid = (start + i + 1).clamp(start + 1, end - 1);
                break;
            }
        }
        boxes[pick] = (start, mid);
        boxes.push((mid, end));
    }

    let mut palette = Vec::with_capacity(boxes.len());
    let mut index_of = vec![0u8; BINS];
    for (i, (start, end)) in boxes.iter().enumerate() {
        let mut count = 0u64;
        let mut sum = [0u64; 3];
        for bin in &bins[*start..*end] {
            count += u64::from(bin.count);
            for (total, v) in sum.iter_mut().zip(bin.sum) {
                *total += u64::from(v);
            }
            index_of[bin.bucket as usize] = i as u8;
        }
        palette.push(sum.map(|total| {
            // An empty box only happens when the frame itself is empty.
            total.checked_div(count).unwrap_or(0) as u8
        }));
    }
    (palette, index_of)
}

fn channel(bin: &Bin, axis: usize) -> u32 {
    (bin.bucket >> (12 - 6 * axis as u32)) & 0x3f
}

fn widest_axis(bins: &[Bin]) -> usize {
    (0..3)
        .max_by_key(|axis| {
            let (lo, hi) = extent(bins, *axis);
            hi - lo
        })
        .unwrap_or(0)
}

fn extent(bins: &[Bin], axis: usize) -> (u32, u32) {
    bins.iter().fold((u32::MAX, 0), |(lo, hi), bin| {
        let v = channel(bin, axis);
        (lo.min(v), hi.max(v))
    })
}

fn cost(bins: &[Bin]) -> u64 {
    let pixels: u64 = bins.iter().map(|b| u64::from(b.count)).sum();
    let axis = widest_axis(bins);
    let (lo, hi) = extent(bins, axis);
    pixels * u64::from(hi - lo)
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
    use davimci_present::Overlay;

    fn presentation(width: u32, height: u32, fill: [u8; 4]) -> Presentation {
        let surface = Resolution { width, height };
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            pixels.extend_from_slice(&fill);
        }
        Presentation {
            video: None,
            surface,
            pixels: Arc::new(pixels),
            pixels_id: 1,
            quad: letterbox(surface, surface),
            position: None,
            overlay: Overlay::default(),
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
        // One colour, so one register, declared then selected; both rows are
        // set, so the bits are 0b11 -> '?'+3. One column is shorter than the
        // repeat form, so it is written plainly.
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            "\x1bPq\"1;1;1;2#0;2;100;0;0#0B-\x1b\\"
        );
    }

    /// The repeat form and the short-row rule are what keep a preview frame
    /// inside a pty: a flat band is a handful of bytes, not one per column.
    #[test]
    fn flat_runs_collapse_to_the_repeat_form() {
        let image = Image {
            width: 10,
            height: 1,
            rgb: [17u8, 34, 51].repeat(10),
        };
        assert_eq!(
            String::from_utf8_lossy(&sixel(&image)),
            "\x1bPq\"1;1;10;1#0;2;7;13;20#0!10@-\x1b\\"
        );

        // Four or fewer is written plainly, since `!4@` is no shorter.
        let mut out = Vec::new();
        run_length(&[1, 1, 1, 1, 2, 2, 2, 2, 2, 2], &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "@@@@!6A");

        // A colour that covers only the left of the band ends its row there
        // rather than writing empty columns.
        let mut out = Vec::new();
        run_length(&[3, 3], &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "BB");
    }

    /// The whole point of the adaptive palette: a picture with few colours
    /// gets them back exactly, where a fixed cube rounded each to a fifth of
    /// the range.
    #[test]
    fn a_frame_of_few_colours_is_quantised_exactly() {
        let colours = [[13u8, 200, 47], [201, 4, 99], [7, 7, 7]];
        let mut rgb = Vec::new();
        for c in &colours {
            rgb.extend_from_slice(c);
        }
        let image = Image {
            width: 3,
            height: 1,
            rgb,
        };
        let (palette, index_of) = quantise(&image);
        assert_eq!(palette.len(), 3);
        for c in &colours {
            let entry = palette[usize::from(index_of[bucket(*c)])];
            assert_eq!(entry, *c, "{c:?} came back as {entry:?}");
        }
    }

    /// A gradient reaches the histogram's own resolution rather than the
    /// palette's, and every pixel still lands within a few levels of itself -
    /// the fixed cube was out by up to 25.
    #[test]
    fn a_gradient_stays_close_to_the_colours_it_was_given() {
        let mut rgb = Vec::new();
        for i in 0..1024u32 {
            let v = (i * 255 / 1023) as u8;
            rgb.extend_from_slice(&[v, v / 2, 255 - v]);
        }
        let image = Image {
            width: 1024,
            height: 1,
            rgb,
        };
        let (palette, index_of) = quantise(&image);
        let mut worst = 0i32;
        for i in 0..1024u32 {
            let v = (i * 255 / 1023) as u8;
            let want = [v, v / 2, 255 - v];
            let got = palette[usize::from(index_of[bucket(want)])];
            for c in 0..3 {
                worst = worst.max((i32::from(got[c]) - i32::from(want[c])).abs());
            }
        }
        assert!(worst <= 4, "a gradient pixel was out by {worst} levels");
    }

    /// More distinct colours than sixel has registers: the palette stops at
    /// the format's limit instead of overrunning it.
    #[test]
    fn a_frame_of_many_colours_fills_the_palette_and_no_more() {
        let mut rgb = Vec::new();
        for i in 0..4096u32 {
            // Spread over the cube rather than along a line, so the bins do
            // not collapse the way a gradient's do.
            rgb.extend_from_slice(&[
                (i * 37 % 256) as u8,
                (i * 91 % 256) as u8,
                (i * 151 % 256) as u8,
            ]);
        }
        let image = Image {
            width: 4096,
            height: 1,
            rgb,
        };
        let (palette, index_of) = quantise(&image);
        assert_eq!(palette.len(), MAX_COLOURS);
        assert!(index_of.iter().all(|i| usize::from(*i) < palette.len()));
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

#[cfg(test)]
mod perf {
    use super::*;

    /// A band the width of a wide terminal, full of distinct colours: the
    /// pathological case for both the palette and the payload.
    ///
    /// The budget is the pty, not the CPU. Frames that miss it are dropped
    /// rather than queued, so the number that matters is how much a terminal
    /// has to parse before the next one is ready.
    #[test]
    #[ignore = "perf"]
    fn a_worst_case_sixel_band_encodes_within_its_budget() {
        let (w, h) = (1600u32, 300u32);
        let mut rgb = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
            }
        }
        let image = Image {
            width: w,
            height: h,
            rgb,
        };
        let start = std::time::Instant::now();
        let bytes = sixel(&image);
        let took = start.elapsed();
        println!("{w}x{h} -> {} bytes in {took:?}", bytes.len());
        assert!(
            took < std::time::Duration::from_millis(40),
            "encode took {took:?}"
        );
        assert!(bytes.len() < 400_000, "payload was {} bytes", bytes.len());
    }
}
