# GPU plan

Preview and export performance work that involves the GPU. Split out of
`todo.md`, which now points here.

Today the GPU is a blitter: MLT decodes and composites in system memory,
`davimci-present` CPU-blits the composition into an RGBA buffer, and the host
uploads that buffer as a texture (`davimci-gui`, `davimci-cli`). No hardware
decode, no hardware encode, no GPU filters.

## Goal

CPU-only stays a first-class, fully supported path. GPU acceleration is an
opt-in fast path that must produce the *same edit result* as CPU. Nothing here
may make a machine without a usable GPU worse off, including CI and the
lavapipe snapshot runs.

## Rules this work must not break

- Nothing outside `davimci-mlt` may reference MLT types. Hardware-decode
  plumbing is a backend detail behind `RenderBackend`; `PreviewScale` and
  `VideoFrame` are the only preview vocabulary the rest of the tree sees.
- `davimci-core`, `davimci-cmd` and `davimci-motion` stay testable with no
  window, GPU, or media.
- Acceleration is never a `Command`. It changes how pixels are produced, never
  what the timeline holds, so it never enters the undo log.
- Every acceleration failure is a *recoverable* error: fall back to the CPU
  path, keep editing alive, and say so in one complete sentence. A missing
  VAAPI driver is not a corruption exit.
- Export correctness outranks export speed. A hardware encoder that cannot hit
  the preset's codec/profile is refused before the job starts, not silently
  substituted.

## Measure first

No phase below lands without a before/after number from the same clip on the
same machine. Wanted, as a `criterion` bench or a scripted headless run:

- decode-only ms/frame, 1080p and 4K, H.264 and HEVC long-GOP
- composite ms/frame at 1, 2 and 4 video tracks
- presenter blit ms/frame at `Full`, `Half`, `Quarter`
- host upload ms/frame
- sustained preview fps, and dropped-frame count during a scripted shuttle

Current expectation, to be confirmed rather than trusted: long-GOP 4K decode
dominates, blit plus upload is roughly a third of a 4K frame budget, and
composite is small until track count grows.

## Phases

Ordered by payoff per unit of risk. Do not reorder without a benchmark that
justifies it.

### 1. Hardware decode with CPU-visible frames - landed

MLT's `avformat` producer uses ffmpeg hwaccel (VAAPI on Linux) and frames are
read back into system memory as today.

What is in the tree:

- `davimci-backend::accel` holds the vocabulary - `DecodePolicy` and
  `AccelerationStatus` - and `RenderBackend::set_decode_policy` /
  `acceleration`, both infallible, since an unusable device is recoverable.
- `davimci-mlt::hwaccel::Acceleration` is the probe and the per-source
  decision: a render node must exist, the codec must be long-GOP, and the
  picture must be at least 1280x720, or the source decodes in software. The
  probe takes its device list as an argument, so no-device, wrong-codec and
  mid-session-failure all have unit coverage without a GPU.
- `MltBackend` sets `hwaccel`/`hwaccel_device` on `avformat` producers it
  builds, which works after construction because MLT initialises the video
  codec on the first frame, not on open.
- `:set decode cpu|auto`, defaulting to `cpu`. Changing it rebuilds the
  graph, because a producer that has already decoded has already read the
  property.

First numbers, `counter_1080p60.mkv`, 120 sequential full-scale pulls,
Radeon render node, from `decode_cost_per_frame_is_reported_for_both_paths`:
software 9.52 ms/frame, VAAPI 9.31 ms/frame. Readback plus the RGBA
conversion dominates at 1080p, which is the plan's own warning made concrete
and the reason the default stays `cpu`. 4K long-GOP numbers are still wanted
before the default moves.

Left open:

- 4K long-GOP numbers, and the readback cost measured on its own.
- The hardware frames are not bit-exact with software, so the slow test
  compares them under `HARDWARE_DECODE_TOLERANCE`, which exists for that path
  and no other.

### 2. Cheaper preview pixels

Independent of any GPU work, and the reason a lot of GPU work may prove
unnecessary.

- Done: the proxy machinery (`davimci-analysis::proxy`) is on the runtime
  switch `:set proxy on|off`.
- Automatic `PreviewScale` reduction under sustained frame drops, restored when
  playback catches up. Scrubbing already drops resolution; playback should too.
- Skip the presenter's full-resolution RGBA allocation when nothing but the
  overlay changed. `pixels_id` already lets a host skip the upload; the blit
  itself should be skippable on the same grounds.

### 3. Upload planar YUV, convert in a shader

- Halves or better the bytes crossing PCIe versus RGBA8, and moves colour
  conversion off the CPU.
- Requires the backend to be able to hand out a planar frame, so `VideoFrame`
  grows a format rather than being replaced. RGBA8 remains the default and the
  format all golden tests assert against.
- The presenter's CPU blit still needs an RGBA path for the parity and snapshot
  tests, so this is a host-side addition, not a replacement of `presenter.rs`.

### 4. Zero-copy hardware-decode surface import

Import the decoder's surface straight into `wgpu` (DMA-BUF or Vulkan external
memory) and never touch it with the CPU.

- Deferred, and last. It only removes readback plus upload, which phases 1-3
  have already shrunk or hidden.
- Driver-specific and hard to keep deterministic: GPU-decoded surfaces are not
  guaranteed byte-identical to the software decode, so the golden-pixel and
  host-parity assertions in `crates/davimci-present/src/presenter.rs` cannot
  cover this path. Those tests must keep running against the CPU path, with
  the zero-copy path asserted by a tolerance-based comparison that is
  documented as such and never applied to the CPU path.
- Do not start this without the phase 1-3 numbers in hand.

### 5. Hardware encode for export

- Opt-in per preset, refused rather than downgraded when the encoder cannot
  meet the preset (`crates/davimci-backend/src/preset.rs` already treats preset
  rules as binding, not advisory).
- Quality is a correctness concern here: a hardware encode must still satisfy
  the existing `ffprobe` export assertions, including exact duration and
  stream count.
- Blocked behind the known export bug: `an_exported_file_has_the_duration_of_
  the_timeline` writes a few frames too many on a 5s timeline. Fix that on the
  CPU path first, or a hardware encoder will be blamed for it.

## Configuration surface

One user-visible knob per decision, all runtime, none of them project state:

- `:set decode cpu|auto` - `auto` uses hardware decode where the probe says it
  helps. Default `cpu`: the 1080p numbers do not yet justify `auto`.
- `:set proxy on|off` - phase 2, wired.
- `:set encode cpu|auto` - phase 5.

Environment overrides stay debug aids, not the primary interface. Acceleration
state belongs in whatever `:checkhealth`-style report exists or gets added, so
"why is this slow" is answerable without a log.

## Testing

- `just test` stays free of decode and encode. Every phase here is exercised by
  `--features slow-tests` or a bench, never the fast suite.
- Software decode remains the reference. A GPU path is asserted *against* it,
  and when they disagree the GPU path is wrong until proven otherwise.
- The capability probe needs unit coverage for the failure path: no device, a
  device that opens but cannot decode the codec, and a mid-session failure.
  All three degrade to CPU and produce one user-facing sentence.
- The cross-frontend parity test keeps running on the CPU path. Acceleration
  must not be able to make headless, GUI and TUI diverge, so it must not be
  reachable from the parity fixture's configuration.
- Never loosen an existing tolerance for a GPU path. Add a separate,
  named, documented comparison instead.

## Open questions

- Does MLT's `avformat` producer expose hwaccel usefully at the property level,
  or does a hardware path mean a producer of our own?
- Is MLT's composite step worth moving to the GPU at all, or does the frame
  have to leave VRAM for MLT filters regardless, making the copy unavoidable
  until the backend itself changes?
- What is the minimum GPU we claim to support, and what does the README promise
  for a machine that has none?
