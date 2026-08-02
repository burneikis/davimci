# vimci - Implementation Plan

Companion to `spec.md`. Defines the build order, module boundaries, and the
test strategy for each layer. No schedule or effort estimates are implied by
the ordering; phases are ordered by dependency only.

---

## 0. Technology Choices

| Concern | Choice | Rationale |
|---|---|---|
| Core language | Rust | Memory safety around the MLT C API, strong enum/pattern modelling for modes/commands, good test tooling. |
| Lua runtime | `mlua` (LuaJIT or Lua 5.4) | Sandboxable, mature, maps cleanly to the `require("vimci.*")` namespace. |
| Render backend | `libmlt` via a hand-written `-sys` crate + safe wrapper | Per spec §10.1. |
| Media probing | `ffprobe`/`libavformat` through MLT producers where possible | Avoid a second demux stack. |
| Video presentation | `winit` + `wgpu` textured quad, frames pulled from MLT, audio clock as master | A terminal cannot present real-time video; MLT's own `sdl2` consumer owns its window and can't be composited with our overlays. |
| Primary UI | Single-window GUI: `egui`-on-`wgpu` chrome + custom-painted timeline, sharing the surface with the video quad | Keyboard-first is an input grammar, not a pixel backend. One window, one focus, overlays on the video. |
| Secondary UI | Optional TUI (`ratatui`) in the terminal + a detached, non-focusable video window | Reuses the same presenter crate; useful over SSH-with-display, tiling setups, and as an early checkpoint. |
| Serialization | `serde` + a versioned on-disk format (JSON for the project, binary for analysis cache) | Human-diffable projects, compact caches. |

Workspace layout:

```
crates/
  vimci-core/      timeline model, clips, tracks, grouping, marks, registers
  vimci-cmd/       command objects (apply/invert), undo tree, macro recorder
  vimci-motion/    motions, text objects, jump points, predicate index
  vimci-analysis/  waveform/RMS, silence, scene change, proxy jobs, cache
  vimci-backend/   RenderBackend trait
  vimci-mlt-sys/   raw FFI bindings
  vimci-mlt/       safe wrapper implementing RenderBackend
  vimci-lua/       Lua API surface, config loader, autocmds
  vimci-keys/      key sequence parser (counts, operators, objects), mode FSM
  vimci-app/       frontend-agnostic app state: viewport, zoom, selection, msgs
  vimci-present/   winit+wgpu video surface: texture upload, frame pacing, sync
  vimci-gui/       primary frontend: present + egui chrome + painted timeline
  vimci-tui/       secondary frontend: ratatui timeline + detached present window
  vimci-headless/  scriptable frontend: no window, used by tests and CI
  vimci-cli/       binary, arg parsing, frontend selection, project open/save
```

The hard rule from spec §10.1: nothing outside `vimci-mlt` may reference MLT
types. `vimci-core` must compile and be fully testable with the backend absent.

---

## 0.1 Frontend Strategy (one product, three hosts)

The risk called out explicitly: **three frontends must not become three
implementations.** They are avoided by making the frontends thin.

Everything meaningful lives below the frontend line:

```
            vimci-app  (viewport, zoom, selection, status, pending input)
                 |  Frontend trait: present_frame / draw / poll_input
   +-------------+-------------+--------------------+
   |             |             |                    |
headless        gui           tui            (future frontends)
(no window)  present+egui   ratatui + present-only window
```

- `vimci-app` owns *all* view state - zoom level, scroll offset, which tracks
  are visible, selection highlighting, status-line content, message queue. A
  frontend asks it "what should be on screen" and draws it. No frontend
  computes layout semantics.
- `vimci-present` is the single video path. The GUI hosts it inside its main
  surface; the TUI hosts it in a bare window with no widgets and
  `with_decorations(false)` / focusable disabled, so the terminal keeps
  keyboard focus. **The TUI mode adds a window host, not a renderer.**
- Input always flows through `vimci-keys` into commands. A frontend translates
  platform key events into vimci key tokens and does nothing else with them.

**Priority is explicit:** headless proves the model, the GUI is the product,
the TUI is opt-in. The TUI is a `--features tui` build and is allowed to lag
in capability (no in-video overlays, coarser preview). If it ever demands
divergent core changes, it gets cut rather than accommodated.

---

## Phase 1 - Timeline Model Core (`vimci-core`)

Deliverables:
- `Timeline`, `Track` (video/audio/text/overlay), `Clip`, `Segment`, `Marker`,
  `Mark`, `Register`, `Playhead` (frame position + focused track).
- Frame-based time type (`Frame(u64)` + project framerate) - no floats in the
  model, all rational conversion at the edges.
- Per-clip linkage groups (spec §5), with link/unlink operations.
- Primitive operations, pure and backend-free: `split_at`, `ripple_delete`,
  `lift`, `insert`, `overwrite`, `yank`, `paste`, `move_clip`, `trim_edge`.
- Invariants: no overlapping clips within a track; ripple preserves total
  ordering; group ops keep linked clips frame-aligned.

Testing:
- Unit tests per primitive with hand-built fixture timelines.
- An `assert_invariants(&Timeline)` helper called at the end of every mutation
  in debug builds and in every test.
- Property tests (`proptest`): apply random operation sequences to a random
  timeline, assert invariants always hold and total duration matches an
  independently computed expectation.
- Snapshot tests (`insta`) on a compact textual timeline dump, e.g.
  `V1: [a 0-100][b 100-250]`, so ripple/lift diffs are readable in review.

Exit criteria: split + ripple-delete + lift + paste correct under property
testing, with zero backend code linked.

---

## Phase 2 - Command Layer & Undo Tree (`vimci-cmd`)

Deliverables:
- `trait Command { fn apply(&self, &mut Timeline) -> Result<Effect>; fn invert(&self) -> Box<dyn Command>; }`
  All Phase 1 primitives re-expressed as serializable command structs.
- Undo **tree** with `u`, `Ctrl-r`, `g-`, `g+`, `:undolist` (spec §10.4).
- Snapshot-every-N-commands drift guard (default 100, and on save).
- Repeat register for `.`, macro record/replay buffers for `q`/`@`.
- Project file = last snapshot + command log since it; versioned with a
  migration hook.

Testing:
- Round-trip property test: for a random command sequence,
  `apply` then `invert` in reverse restores a byte-identical serialized state.
- Serialization round-trip for every command variant (enumerated exhaustively
  so a new variant without a test fails to compile via a match on the enum).
- Undo-tree navigation tests: branch, `g-`/`g+` traversal order, redo after
  branching selects the newest branch.
- Drift-guard test: corrupt an `invert` deliberately, assert the snapshot
  bounds the damage to at most N commands.
- Fuzz target over the deserializer for the project format.

Exit criteria: undo/redo/repeat/macros all operate solely through the command
log; no direct timeline mutation path exists outside a command.

---

## Phase 3 - Motions, Jump Points, Text Objects (`vimci-motion`)

Deliverables:
- Motion trait returning a target (frame, track) or a range.
- Built-ins: `h`/`l` (jump points), frame step, `w`/`b`/`e`, `0`/`$`/`gg`/`G`,
  `{`/`}`, `%`, `gt`/`gT`, marks.
- Jump-point engine: computes the point set from zoom level + configured
  sources (clip bounds, markers, silence, peaks) per spec §3.2, cached and
  invalidated on timeline or zoom change.
- Text objects `ic`/`ac`/`it`/`at`/`is` resolving to (range, track scope).
- Predicate motion interface backed by the Phase 5 analysis index; returns
  `Pending` when analysis is incomplete.

Testing:
- Table-driven tests: fixture timeline plus (start position, motion, expected
  landing frame), covering boundary cases - playhead exactly on a boundary,
  at timeline start/end, on an empty track, count prefixes.
- Jump-point determinism test: same timeline + zoom always yields identical
  point sets; monotonic density as zoom increases.
- Text-object scope matrix test: each object x each grouping configuration,
  asserting exactly which tracks the resolved scope touches.

---

## Phase 4 - Key Parser & Mode FSM (`vimci-keys`)

Deliverables:
- Input grammar: `[count] [register] operator [count] motion|textobject`,
  plus standalone commands and `g`-prefixed sequences.
- Modes: NORMAL, VISUAL, VISUAL-LINE, VISUAL-BLOCK, INSERT, COMMAND, with a
  strict transition table and `ModeChanged` events.
- Pending-input state with timeout for ambiguous prefixes (`g`, `d`, counts).
- Visual selection state: anchor/active ends, `o` swap, track-set toggling in
  block mode.
- Keymap table with user override resolution (config over defaults, longest
  match wins).

Testing:
- Golden tests driving key strings through the parser: `"3dw"`, `"d2ic"`,
  `"gs"`, `"\"ayy"`, `"qaxx q@a"`, asserting the produced command sequence.
- Mode-transition property test: no key sequence can leave the FSM in an
  invalid state; `Esc` from any state returns to NORMAL.
- Ambiguity tests: user maps `gx` while `gd` exists; assert correct resolution
  and timeout behavior.
- End-to-end headless tests: feed keys -> assert final timeline snapshot,
  giving executable coverage of the keybinding table in spec §11.

---

## Phase 5 - Media Import & Analysis (`vimci-analysis`)

Deliverables:
- Import pipeline: probe container, expose every audio and subtitle stream in
  an MKV as its own track (spec §7).
- Background job runner with progress reporting to the status line, and
  cancellation on project close.
- Analysis pass: peak + RMS at a 10 ms hop, silence spans, optional
  scene-change keyframes.
- Indexed store enabling O(log n) predicate lookups; sidecar cache at
  `.vimci/cache/<content_hash>.analysis`, versioned and invalidated on bump.
- Proxy generation per spec §10.3, with the `BeforeExport` original-source
  guard.

Testing:
- Fixture media generated at test time by FFmpeg (tone bursts with known
  silence gaps, a color-flip clip for scene change, a 3-audio-track MKV) so no
  large binaries live in the repo; a `just fixtures` target builds them.
- Analysis correctness: known-silence fixture must yield silence spans within
  one hop of ground truth; peak detection must find the exact tone frames.
- Multi-track import: assert track count, per-track stream mapping, and
  subtitle text extraction.
- Cache tests: hit, miss, version-bump invalidation, and corrupted-cache
  recovery (must recompute, never panic).
- Proxy tests: threshold rule selects correctly per resolution/codec matrix;
  proxy framerate and frame count match the source exactly.
- Concurrency test: editing during an in-flight analysis job leaves predicate
  motions returning `Pending`, never stale or wrong results.

---

## Phase 6 - Render Backend (`vimci-backend`, `vimci-mlt-sys`, `vimci-mlt`)

Deliverables:
- `RenderBackend` trait: `probe`, `seek`, `frame_at`, `preview_start/stop`,
  `next_preview_frame`, `audio_clock_position`, `render(job)`, `progress`.
- **Frame-pull preview, not MLT's video window.** Audio realtime output uses
  MLT's `sdl2_audio` / `rtaudio` consumer; video frames are pulled by us as
  RGBA buffers and handed to `vimci-present`. This is the Shotcut pattern and
  is what allows overlays, and what makes GUI and TUI share one video path.
- Preview scaling wired through: request width/height on `mlt_frame_get_image()`
  so scrubbing can drop to half/quarter res, and the TUI's small window is
  cheap by construction.
- Raw bindings; safe wrapper with RAII refcount handling.
- Timeline -> MLT tractor/playlist projection, incremental where possible so
  split/ripple are playlist mutations rather than rebuilds.
- A `MockBackend` implementing the trait deterministically for all upstream
  tests.

Testing:
- Wrapper unit tests under ASan/LeakSanitizer specifically for refcounting
  (spec §10.1 accepted risk): create/clone/drop cycles must show zero leaks.
- Projection tests: for a fixture timeline, assert the generated MLT XML
  matches a golden file - catches ripple/compositing regressions without
  rendering.
- Frame-accuracy tests: render a timecode-burned fixture, decode frames at N
  seek points, OCR-free check via a known per-frame color signature; asserts
  seek lands on the exact frame.
- A/V sync test: click track aligned to a flash frame; assert offset is zero.
- Frame-pull test: pull N consecutive frames headlessly, assert monotonic
  presentation timestamps, no duplicates, and correct pixel signatures.
- Preview-scaling test: quarter-res pull yields the expected dimensions and
  the same frame content, scaled - never a different frame.
- Render smoke test per export preset, gated behind a `--features slow-tests`
  flag so the default suite stays fast.

---

## Phase 7 - Lua Config & Plugin API (`vimci-lua`)

Deliverables:
- Config loader honoring `~/.config/vimci/init.lua`, `keymaps.lua`,
  `motions/`, `presets/`, `plugin/`, and opt-in `.vimci.lua` project-local
  overrides with an explicit trust prompt.
- Modules: `vimci.keymap`, `vimci.motions`, `vimci.textobject`, `vimci.export`,
  `vimci.timeline`, `vimci.media`, `vimci.autocmd`, `vimci.editor`.
- Event dispatch for the v1 event list (`PlayheadMoved`, `SplitPerformed`,
  `ClipDeleted`, `ClipInserted`, `ModeChanged`, `BeforeExport`, `AfterExport`,
  `ProjectLoaded`).
- Error isolation: a throwing user callback logs and is disabled for the
  session rather than crashing the editor.

Testing:
- Every code snippet in spec §9 becomes a test fixture that must load and
  behave as documented - the spec is the acceptance suite.
- Lua-driven integration tests: a `.lua` file registers a custom motion and
  keymap, harness feeds keys, asserts resulting timeline state.
- Hook ordering and cancellation: a `BeforeExport` handler returning an error
  must abort the render.
- Sandbox tests: project-local `.vimci.lua` is not executed without trust;
  untrusted config cannot reach `os.execute`/`io` by default.
- Panic-safety test: user callback that throws leaves the editor usable and
  the timeline unmodified.

---

## Phase 8 - Export (`vimci-export` within `vimci-cli`)

Deliverables:
- Preset registry (built-in + Lua-defined), validation with clear errors.
- Track selection at export: per-audio-track and per-subtitle-track include /
  burn-in / sidecar options; multi-track MKV passthrough.
- `:render <preset>` and `:export <path> --preset <name>`.
- Progress + cancellation; `BeforeExport`/`AfterExport` hook points; the
  proxy-relink guard.

Testing:
- Preset validation table tests, including invalid codec/container pairings.
- Output verification via `ffprobe`: stream count, codecs, resolution, frame
  rate, duration, and subtitle disposition match the preset.
- Multi-track MKV round-trip: export then re-import, assert the track graph is
  isomorphic to the original.
- Guard test: force a clip onto a proxy, assert export fails with the specific
  proxy error rather than silently rendering low-res.

---

## Phase 9a - View State (`vimci-app`)

Built before any frontend, so no frontend can invent its own.

Deliverables:
- Viewport model: zoom level, scroll offset, visible time range, visible track
  range, and the derived jump-point tick positions for the ruler.
- Selection/highlight description, current-track indicator, mode + scope
  string (`-- VISUAL (V1,A2) --`), message/notification queue, job progress.
- `Frontend` trait and an app event loop that is generic over it.
- Scroll-follow rules: playhead must remain within the viewport after any
  motion; zoom anchors on the playhead.

Testing:
- Pure unit tests, no rendering: viewport arithmetic, zoom anchoring,
  scroll-follow, tick-position generation at each zoom level.
- Property test: after any random motion or zoom sequence, the playhead is
  inside the visible range and the viewport is within timeline bounds.
- Snapshot tests on a textual dump of the view state, shared as the golden
  input for every frontend's rendering tests.

---

## Phase 9b - Video Presenter (`vimci-present`)

One crate, two hosts. This is the anti-duplication keystone.

Deliverables:
- `winit` + `wgpu` surface, RGBA texture upload, aspect/letterbox handling.
- Frame pacing against the backend's audio clock; drop-late / repeat-on-starve
  policy with counters exposed for tests.
- Two host modes:
  - `Embedded` - renders into a surface owned by the GUI, alongside egui.
  - `Detached` - owns a bare undecorated, non-focusable window for TUI mode,
    so the terminal never loses keyboard focus.
- Optional overlay layer (playhead timecode, safe areas) in `Embedded` only.
- A `HeadlessPresenter` that writes frames to memory for tests.

Testing:
- Frame-pacing tests against `MockBackend` with a synthetic clock: assert
  dropped/repeated frame counts match expectation for fast, slow, and jittery
  sources.
- Image-diff snapshot tests via `wgpu` offscreen rendering: known input frame
  in, golden PNG out, with a small perceptual tolerance.
- Letterbox/aspect matrix test across source and window aspect ratios.
- Host-parity test: the same input frame through `Embedded` and `Detached`
  produces identical video pixels - proves the paths have not diverged.
- Detached-window focus behavior is a manual checklist item (i3, floating).

---

## Phase 9c - GUI Frontend (`vimci-gui`) - primary

Deliverables:
- Single window: video quad (`vimci-present` embedded) + custom-painted
  timeline (tracks, clips, ruler with jump-point ticks, playhead, selection)
  + `egui` chrome.
- Status line, command line (`:`) with history and completion.
- Media picker for `i`/`a`/`r`; text-edit INSERT mode for subtitle clips;
  clip properties panel (spec §8 transforms).
- Key event translation into `vimci-keys` tokens; nothing else.

Testing:
- Image-diff snapshot tests of the full window at fixed sizes, covering each
  mode, selection kind, and panel state.
- Layout tests at extreme sizes (very short, very narrow, more tracks than
  fit) asserting no panic and a sane viewport.
- Input-translation tests: synthetic `winit` events produce the expected key
  tokens, including modifiers and `<Left>`/`<Right>`.
- Golden view-state reuse: renders are driven by the Phase 9a fixtures, so a
  view-state regression fails in both `vimci-app` and here.

---

## Phase 9d - TUI Frontend (`vimci-tui`) - optional, `--features tui`

Explicitly a stepping stone and a nice-to-have. Ships only if it stays thin.

Deliverables:
- `ratatui` timeline, ruler with tick marks, status line, command line -
  rendered from the same `vimci-app` view state as the GUI.
- Preview via `vimci-present` in `Detached` mode; `:set preview off` for
  no-display sessions.
- Terminal key translation into `vimci-keys` tokens.
- Documented limitations: no in-video overlays, no properties panel (falls
  back to command mode `:set clip.*`), coarser timeline resolution.

Testing:
- Terminal snapshot tests (`ratatui` test backend) at fixed sizes per mode.
- **Cross-frontend parity test:** one scripted session driven through headless,
  GUI, and TUI must produce an identical final timeline snapshot and identical
  `vimci-app` view state. This is the test that keeps three hosts from becoming
  three products; any divergence is a bug in the frontend, never in core.
- Degradation test: with preview disabled and no display available, the TUI
  still starts and all editing works.

---

## Phase 10 - Integration & Hardening

Deliverables:
- Headless scripted-session runner: a file of keystrokes plus assertions,
  usable as both a test format and a debugging tool.
- Crash recovery: autosave of the command log; recover on next open.
- Performance baselines on a large project (hundreds of clips, several tracks).

Testing:
- Full-workflow integration tests mirroring spec §1: import multi-track MKV ->
  ripple-delete sections -> mute/trim one audio track -> add an overlay ->
  add subtitles -> export; assert final `ffprobe` output and a golden timeline
  snapshot.
- Soak/fuzz session: random key sequences against a fixture project asserting
  no panic, invariants hold, and undo returns to the initial state exactly.
- Benchmarks (`criterion`) with regression thresholds for: jump-point
  computation, ripple delete on a large timeline, predicate motion lookup,
  undo of a long log, and project load.
- Memory-leak run of a long editing session under valgrind/ASan.

---

## Cross-Cutting Test Strategy

| Layer | Primary technique |
|---|---|
| Timeline model | Property tests + invariant assertions |
| Commands/undo | Apply/invert round-trip properties, serialization fuzz |
| Motions/objects | Table-driven landing-position tests |
| Key parsing | Golden key-string -> command-sequence tests |
| Analysis | Generated fixture media with known ground truth |
| MLT wrapper | Sanitizer-backed refcount tests, golden XML projection |
| Lua API | Spec snippets as executable acceptance tests |
| Export | `ffprobe` assertions on real output |
| View state | Pure viewport/zoom unit + property tests |
| Presenter | Offscreen image-diff snapshots, synthetic-clock pacing tests |
| GUI | Window image-diff snapshots + input translation tests |
| TUI | Terminal snapshot tests + cross-frontend parity |
| Whole app | Scripted headless sessions + soak fuzzing |

Standing rules:
1. `vimci-core` and `vimci-cmd` have no backend or I/O dependency and must
   stay 100% unit-testable in-process.
2. Every bug fix lands with a regression test naming the issue.
3. Default `cargo test` must be fast; anything requiring real decode/encode is
   behind `--features slow-tests` and runs in CI only.
4. Test media is generated, never committed.
5. CI matrix: default suite, slow-tests suite, sanitizer build, clippy +
   rustfmt, a Lua-config compatibility suite that loads every example config
   in the docs, and a headless-GPU (`lavapipe`) job for presenter and GUI
   snapshot tests.
6. No frontend may contain view logic. If a fix needs to land in both
   `vimci-gui` and `vimci-tui`, it belongs in `vimci-app` or `vimci-present`
   instead - the cross-frontend parity test exists to catch this.
7. The GUI is the reference frontend. Headless and TUI are validated against
   it, not the reverse.

---

## Milestones

| M | Definition of done |
|---|---|
| M1 | Headless: load a fixture timeline, move playhead, split, ripple delete, undo - all via keys, verified by snapshot tests. No window code exists yet. |
| M2 | Import a multi-track MKV; frames pull from MLT into `vimci-present` and play in sync with audio in a bare window. No editing UI - proves the video path. |
| M3 | GUI: timeline + video in one window, scrub with jump points, full cut workflow, export a multi-audio MKV with a working preset. **This is the first genuinely usable build.** |
| M4 | Lua config fully wired: custom motions, text objects, keymaps, hooks, export presets. |
| M5 | Overlays and subtitle tracks editable and exportable (burn-in and sidecar). |
| M6 | Optional TUI frontend behind `--features tui`, passing cross-frontend parity. Cut without regret if it is not thin. |
| M7 | Hardened: soak-tested, benchmarked, crash recovery, documented default keymap. |

The ordering rule: **nothing before M3 is a product.** M6 is deliberately
after export, Lua, and subtitles - the TUI is a convenience, and shipping it
early would mean maintaining two frontends through every core change.
Correspondingly, `vimci-app` and `vimci-present` are built with two hosts in
mind from the start (cheap), but only one host is *implemented* until M6
(avoids the three-implementations trap).

---

## Deferred (tracked, not in v1)

- GPU preview path (spec §10.6) - largely resolved by the `wgpu` presenter;
  what remains deferred is zero-copy hardware-decode surface import.
- Terminal-inline preview (kitty/sixel via `ratatui-image`) as a fallback for
  the TUI when no window can be opened. Explicitly low priority: escape-
  sequence throughput gives no frame-pacing guarantees, so it can never be the
  primary path.
- Custom subtitle layout engine vs. MLT built-in producers (spec §10.6).
- Beat detection as a jump-point source.
- ML-based scene detection hook.
- Plugin distribution/package manager.
