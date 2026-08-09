# GPU plan

Preview and export performance work that involves the GPU. Split out of
`todo.md`, which now points here.

Phases 1, 2, 3 and 5 have landed; phase 4 is blocked by MLT and is described
below as such. Each phase records the measurement that justified it, and each
of the three runtime switches - `:set decode`, `:set proxy`, `:set encode` -
defaults to the CPU path.

Where the CPU still is: MLT decodes and composites in system memory, and the
CPU composition in `davimci-present` remains the reference every parity,
snapshot and TUI path uses. The GPU path is an addition beside it, never a
replacement.

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

### 2. Cheaper preview pixels - landed

- The proxy machinery (`davimci-analysis::proxy`) is on the runtime switch
  `:set proxy on|off`.
- `davimci-present::adaptive` drops `PreviewScale` a step per second of
  sustained drops and gives it back a step per clean second, restoring
  everything it took when playback stops. The policy is a pure function of
  the pacing counters, so all of it is proved without a backend; the editor
  applies it by restarting the pass through `Transport::rescale`. It never
  reduces a scale the user chose - only one it chose itself.
- The presenter reuses its composed buffer whenever the picture is the same,
  decided by a `Pacer` epoch rather than by frame position, and rebuilds only
  the overlay. Same buffer means the same `pixels_id`, so the host skips the
  upload as well as the blit. `blits_skipped` counts it.

### 3. Upload planar YUV, convert in a shader - landed for stills

The number that justified it, 1080p60, 120 sequential pulls: an RGBA pull
costs 8.24 ms/frame and a planar pull of the same picture costs 1.55 ms.
Almost all of what looked like decode cost was MLT converting to RGBA with
swscale. The upload also drops from 8 294 400 bytes to 3 110 400.

What is in the tree:

- `davimci-backend::frame` grows `PixelFormat` and `PlanarFrame`, with
  `to_rgba` as the CPU reference conversion (BT.709 limited range, integer
  arithmetic, no floats). RGBA8 stays the default and the format every golden
  test asserts against.
- `RenderBackend::supports_planar` / `planar_frame_at`; `MltBackend` pulls
  `mlt_image_yuv420p` and hands out the three planes.
- `davimci-present::gpu::PlanarRenderer` (feature `gpu`) uploads three
  R8Unorm textures and converts in WGSL. It is asserted against the CPU
  conversion by `crates/davimci-present/tests/gpu.rs`, which runs on any
  adapter including lavapipe and skips loudly where there is none.
- `Presenter::present_planar` letterboxes and builds the overlay without
  composing anything: `Presentation::video` carries the frame and `pixels` is
  empty. The RGBA path is untouched, and it is what the parity, snapshot and
  TUI paths still use.
- The window installs the renderer when eframe gives it a `wgpu` render
  state, and draws the planar frame into the quad the presenter computed.

Two tolerances, both named and both belonging to this path alone:
`SHADER_TOLERANCE` for the matrix, and `CHROMA_UPSAMPLE_TOLERANCE` for the
one deliberate difference - the shader interpolates chroma where the CPU
reference repeats it.

Left open: playback. Preview frames arrive through the consumer's
`consumer-frame-show` listener as RGBA, so the planar path currently covers
stills - the playhead, scrubbing, paused editing - and playback still pays
the conversion. Extending it means the listener lifting `yuv420p` and
`VideoFrame` carrying either format through the pacer, which is a change to
the queue every frontend shares and wants its own before/after number.

### 4. Zero-copy hardware-decode surface import

Import the decoder's surface straight into `wgpu` (DMA-BUF or Vulkan external
memory) and never touch it with the CPU.

**Blocked by the backend, not by effort.** MLT hands out system memory:
`mlt_frame_get_image` returns a CPU buffer in every format davimci can ask
for, and the one texture format in `mlt_image_format` is movit's internal
one. There is no surface to import while MLT composites, which answers the
second open question below - the frame has to leave VRAM for MLT's filters
regardless. Phase 4 is therefore a `RenderBackend` change, not a flag, and
phase 3 has already removed most of what it would have saved.

- Deferred, and last. It only removes readback plus upload, which phases 1-3
  have already shrunk or hidden.
- Driver-specific and hard to keep deterministic: GPU-decoded surfaces are not
  guaranteed byte-identical to the software decode, so the golden-pixel and
  host-parity assertions in `crates/davimci-present/src/presenter.rs` cannot
  cover this path. Those tests must keep running against the CPU path, with
  the zero-copy path asserted by a tolerance-based comparison that is
  documented as such and never applied to the CPU path.
- Do not start this without the phase 1-3 numbers in hand.

### 5. Hardware encode for export - landed

The export duration bug this was blocked behind no longer reproduces:
`an_exported_file_has_the_duration_of_the_timeline` counts exactly the
timeline's frames.

- `HardwareEncode { Off, Preferred, Required }` travels on `RenderSettings`.
  A preset opts in with `hardware = true`, which is validated where the
  preset is defined: a codec with no hardware encoder at all - ProRes - is
  rejected there rather than after a long render.
- `:set encode cpu|auto` is the session policy. `auto` accelerates where the
  encoder meets the preset; a preset that *requires* hardware is binding and
  is refused before the job starts, with no partial file.
- The substitution keeps the codec: `libx264` becomes `h264_vaapi`,
  `libx265` becomes `hevc_vaapi`, `libvpx-vp9` becomes `vp9_vaapi`. A
  hardware encode therefore still satisfies every `ffprobe` assertion, which
  `a_hardware_export_meets_the_same_assertions_as_a_software_one` checks:
  same codec, one video stream, exactly the timeline's frames.
- **A render node is not an encode entrypoint.** The first version of this
  trusted the device probe and produced a container with no header on a card
  that decodes H.264 and cannot encode it. The encoder is now proved by
  encoding: two frames to a temporary file that must decode back, once per
  session per encoder and device. That is what the machine this landed on
  needed - it has a render node, VAAPI decode, and no H.264 encode
  entrypoint, so `:set encode auto` there correctly exports in software.

## Configuration surface

One user-visible knob per decision, all runtime, none of them project state:

- `:set decode cpu|auto` - `auto` uses hardware decode where the probe says it
  helps. Default `cpu`: the 1080p numbers do not yet justify `auto`.
- `:set proxy on|off` - wired.
- `:set encode cpu|auto` - wired. Default `cpu`.

`Editor::acceleration()` returns the whole state as one complete sentence,
which is the `:checkhealth`-shaped answer to "why is this slow" until such a
report exists.

Environment overrides stay debug aids, not the primary interface. Acceleration
state belongs in whatever `:checkhealth`-style report exists or gets added, so
"why is this slow" is answerable without a log.

## Testing

- `just test` stays free of decode and encode. Every phase here is exercised by
  `--features slow-tests`, `just test-gpu`, or a bench - never the fast suite.
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

## Answered

- **Does MLT's `avformat` producer expose hwaccel at the property level?**
  Yes: `hwaccel` and `hwaccel_device`, read when the video codec is
  initialised on the first frame, which is after the producer is constructed.
  No producer of our own is needed.
- **Is MLT's composite step worth moving to the GPU?** Not reachable: MLT
  hands out system memory, so the frame leaves VRAM regardless. It is a
  backend change, which is what phase 4 now depends on.
- **Where did the preview cost actually go?** Not decode - colour conversion.
  Skipping MLT's RGBA conversion is worth more than hardware decode was
  (8.24 ms/frame to 1.55), which is why phase 3 outran phase 1 in payoff.

## Still open

- 4K long-GOP decode numbers, and the readback cost measured on its own.
- Planar frames through the playback queue, not just stills.
- What is the minimum GPU we claim to support, and what does the README promise
  for a machine that has none?
