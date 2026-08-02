# vimci - Implementation Plan

Companion to `spec.md`. Defines the build order, module boundaries, and the
test strategy for each layer. No schedule or effort estimates are implied by
the ordering; phases are ordered by dependency only.

---

## 0. Technology Choices

| Concern | Choice | Rationale |
|---|---|---|
| Core language | Rust | Memory safety around the MLT C API, strong enum/pattern modelling for modes/commands, good test tooling. |
| Lua runtime | `mlua` with **vendored Lua 5.4** (not system Lua) | System Lua on Arch is 5.5, which `mlua` does not support. Vendoring pins the version and makes builds reproducible. LuaJIT is a build-time alternative. |
| Render backend | `libmlt` via a hand-written `-sys` crate + safe wrapper | Per spec §10.1. |
| Media probing | `ffprobe`/`libavformat` through MLT producers where possible | Avoid a second demux stack. |
| Video presentation | `winit` + `wgpu` textured quad, frames pulled from MLT, audio clock as master | A terminal cannot present real-time video; MLT's own `sdl2` consumer owns its window and can't be composited with our overlays. |
| Primary UI | Single-window GUI: `egui`-on-`wgpu` chrome + custom-painted timeline, sharing the surface with the video quad | Keyboard-first is an input grammar, not a pixel backend. One window, one focus, overlays on the video. |
| Secondary UI | Optional TUI (`ratatui`) in the terminal + a detached, non-focusable video window | Reuses the same presenter crate; useful over SSH-with-display, tiling setups, and as an early checkpoint. |
| Serialization | `serde` + a versioned on-disk format (JSON for the project, binary for analysis cache) | Human-diffable projects, compact caches. |
| Errors | `thiserror` in libraries, `anyhow` only at the binary edge | Typed errors are required by the recovery policy in Phase 0. |
| License | GPL-3.0, dynamically linking LGPL-2.1 `libmlt` | Per spec §13. Never static-link MLT; never vendor `melt`. |

### Build prerequisites (Arch)

```sh
sudo pacman -S --needed mlt ffmpeg clang rust vulkan-swrast
```

`mlt` ships its headers in the main package, so no separate `-dev` package is
needed. `vulkan-swrast` (lavapipe) is only required to run presenter/GUI
snapshot tests without a GPU. Verified present on a stock Arch install:
`ffmpeg`, `ffprobe`, `cargo`, `rustc`, `clippy`, `rustfmt`, `clang`.

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

## Phase 0 - Error Handling & Recovery Strategy

Decided before any code, because it dictates function signatures everywhere and
is miserable to retrofit.

**Four error classes, each with a fixed policy:**

| Class | Example | Policy |
|---|---|---|
| **User error** | Trim past a clip's handles, transition without enough frames, bad `:command` | Reject the command *before* it mutates. Status-line message. Never enters the undo log. |
| **Missing/offline media** | Source file moved between sessions | Project still opens. Clips flagged `Offline`, render as a placeholder, editing allowed. `:relink` fixes. Export refuses while any clip is offline. |
| **Recoverable runtime** | Decode failure on one frame, analysis job crash, Lua callback throws | Degrade locally: black frame, mark analysis `Failed`, disable the offending Lua handler for the session. Log, notify, keep editing. |
| **Corruption / bug** | Failed invariant, deserialization failure, backend panic | Do not continue on a corrupt timeline. Flush the autosave log, report, exit cleanly. The Phase 2 snapshot bounds the loss. |

**Rules this imposes:**

1. `Command::apply` is **validate-then-mutate**: all checks run first, so a
   rejected command leaves the timeline byte-identical. This is what makes the
   undo log trustworthy.
2. No `unwrap`/`panic` in library crates outside invariant assertions - enforced
   by a clippy lint at deny level.
3. FFI boundaries catch panics (`catch_unwind`) so a C-side fault cannot unwind
   through Rust.
4. Every error carries user-facing text; no raw `Debug` output reaches the
   status line.

Testing:
- Each error class gets a fault-injection test proving the policy holds.
- Property test: a rejected command never modifies the timeline.
- Offline-media test: move a fixture file, reopen, assert the project loads,
  edits work, and export fails with the specific offline error.

---

## Phase 1 - Timeline Model Core (`vimci-core`)

Deliverables:
- `Timeline`, `Track` (video/audio/text/overlay), `Clip`, `Segment`, `Marker`,
  `Mark`, `Register`, `Playhead` (frame position + focused track).
- Frame-based time type (`Frame(u64)` + single project framerate per spec §7.1)
  - no floats in the model, all rational conversion at the edges.
- Timeline properties (fps, resolution, sample rate) as project-level state,
  with the conform rules that every clip is validated against.
- Clip properties: gain, fades, transform, and link group - stored on the clip,
  applied as render-time filters, never destructive.
- Per-clip linkage groups (spec §5), with link/unlink operations.
- Primitive operations, pure and backend-free: `split_at`, `ripple_delete`,
  `lift`, `insert`, `overwrite`, `yank`, `paste`, `move_clip`, plus the full
  trim family (`ripple_trim`, `roll`, `slip`, `slide`) from spec §4.0.1.
- Invariants: no overlapping clips within a track; ripple preserves total
  ordering; group ops keep linked clips frame-aligned; every clip duration is a
  whole number of timeline frames.

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
- `trait Command { fn apply(&self, &mut Timeline) -> Result<Effect>; fn describe(&self) -> String; }`
  where `Effect { applied, inverse }`. **Amended during implementation:** the
  original sketch had `fn invert(&self) -> Box<dyn Command>`, which cannot
  work - the inverse of a ripple delete is "restore *these* clips", known only
  after the delete runs. The inverse is therefore returned by `apply`.
  `Effect::applied` is the command as executed, with every id pinned, and is
  what the log stores so redo is byte-exact.
  All Phase 1 primitives re-expressed as one serializable `EditCommand` enum.
- Commands never mint an id the log does not record: an edit that incidentally
  cuts a clip expands into a `Sequence` with an explicit `Split` in front, and
  undo joins those cuts back up. A rejected command also hands back reserved
  ids, since the id cursor is part of the serialized state.
- Phase 1 gained the four model primitives these inverses need: `join_at`,
  id-preserving `restore`, `set_group`, and `set_clip_props`.
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

Status: complete. Macros store opaque input tokens rather than commands, so
replay is keystroke-shaped as in vim; `vimci-keys` (Phase 4) gives the tokens
meaning. History itself is not yet persisted - a reopened project starts a
fresh undo tree from the saved state.

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

Status: complete. Motions and objects are pure queries - they resolve a target
and never mutate, so a verb can validate before building a command. The
jump-point set is memoised behind a fingerprint of everything it reads, so a
stale hit is not representable. Predicate motions go through the
`PredicateIndex` trait and report `Pending` until Phase 5 implements it;
`ac` currently resolves to the same range as `ic` and widens on its own once
Phase 9f adds transitions (spec §4.1).

---

## Phase 4 - Key Parser & Mode FSM (`vimci-keys`)

Deliverables:
- Input grammar: `[count] [register] operator [count] motion|textobject`,
  plus standalone commands, `g`-prefixed sequences, and `<Space>` leader
  sequences (spec §3.2.1).
- Transport bindings: `<Space><Space>` play/pause, `J`/`K`/`L` shuttle,
  `<Space>p` preview-and-return, `<Space>l` loop selection. Transport dispatches
  to the backend clock, **not** through the undo log - playback is not an edit.
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

## Phase 5 - Media Import, Conform & Analysis (`vimci-analysis`)

Deliverables:
- Import pipeline: probe container, expose every audio and subtitle stream in
  an MKV as its own track (spec §7).
- **Conform stage (spec §7.1):** framerate retime, resolution scale with
  letterbox/crop policy, audio resample - so everything downstream sees a
  single-rate, single-resolution timeline. Project fps/resolution defaults from
  the first import.
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
- Conform matrix test: import 23.976 / 25 / 30 / 60 fps and 720p / 1080p / 4K /
  anamorphic fixtures into a 1080p60 timeline; assert exact whole-frame
  durations, correct scaling, and no cumulative drift over a long clip.
- Re-conform test: change `timeline.fps` with clips present, assert it is a
  single undoable command that restores exactly on undo.
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

## Phase 8 - Project Lifecycle (`vimci-cli`)

Deliverables (spec §12):
- `:w`, `:q`, `:q!`, `:wq`, `:e`, `:new`, `:ls`, `:bn`/`:bp`/`:b <n>`.
- Multiple open timelines with global registers and marks shared across them.
- Continuous autosave of the command log to `.vimci/autosave/`, never touching
  the project file; crash recovery prompt on next open.
- `:relink` for offline media (Phase 0 policy).

Testing:
- Save/load round-trip: byte-identical timeline state after reload.
- Dirty-state tests: `:q` refuses with unsaved changes, `:q!` discards.
- Cross-timeline yank/paste test.
- Crash-recovery test: kill the process mid-session, reopen, assert the log
  replays to the exact pre-kill state.
- Format-migration test: load a project written by an older schema version.

---

## Phase 8b - Export (`vimci-export` within `vimci-cli`)

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

## Phase 9e - Audio Operations (`vimci-core` + `vimci-mlt`)

Deferred to here deliberately (spec §6.1): these are clip properties applied as
render-time filters, so they need the backend and a UI to be worth having.

Deliverables:
- Track mute (`<Space>m`) and solo (`<Space>s`).
- Clip/selection gain (`+`/`-`, `:gain`), `:normalize`, `:duck`.
- Fades (`f` + motion, `:fade`) with shape options.
- Waveform display on audio tracks, reusing the Phase 5 analysis data.
- Analysis-cache invalidation when gain or fades change.

Testing:
- Property tests that gain/fade are non-destructive and exactly invertible.
- Render-and-measure: apply a known gain, render, assert measured RMS matches
  the target within tolerance.
- Fade shape verification by sampling the rendered envelope.
- Solo/mute matrix test across multi-track fixtures.
- Cache-invalidation test: change gain, assert predicate motions report stale
  until `:analyze` re-runs.

---

## Phase 9f - Transitions (`vimci-core` + `vimci-mlt`)

Deliverables (spec §6.2):
- Transition objects occupying a clip overlap; `gx`, `:transition`, `dax`.
- Handle-frame validation with a clear failure when handles are insufficient
  (Phase 0 user-error class - reject before mutating).
- Mapping onto MLT transitions; Lua-extensible registry.
- Makes the `ac` text object (spec §4.1) fully meaningful.

Testing:
- Handle-availability tests: sufficient, insufficient, and exactly-enough.
- `ac` object test: assert it now spans clip + transition.
- Ripple-with-transition tests: deleting a neighbour resolves the transition
  sanely rather than orphaning it.
- Golden MLT XML projection for each transition type.

---

## Phase 10 - Integration & Hardening

Deliverables:
- Headless scripted-session runner: a file of keystrokes plus assertions,
  usable as both a test format and a debugging tool.
- Crash recovery: autosave of the command log; recover on next open.
- Performance validation against spec §14: **1080p60 playback and editing must
  be smooth**; split/ripple/undo instant on a few hundred clips; predicate
  motions never scan. Coarse targets, measured before any optimization.

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
| M3 | GUI: timeline + video in one window, playback and shuttle, scrub with jump points, trim, full cut workflow, save/load, export a multi-audio MKV. **This is the first genuinely usable build.** |
| M4 | Lua config fully wired: custom motions, text objects, keymaps, hooks, export presets. |
| M5 | Audio operations: mute, solo, gain, fades, waveforms - completing workflow step 3. |
| M6 | Overlays, subtitle tracks, and transitions editable and exportable. |
| M7 | Optional TUI frontend behind `--features tui`, passing cross-frontend parity. Cut without regret if it is not thin. |
| M8 | Hardened: soak-tested, 1080p60 validated, crash recovery, documented default keymap. |

The ordering rule: **nothing before M3 is a product.** M7 is deliberately
last-but-one - the TUI is a convenience, and shipping it early would mean
maintaining two frontends through every core change.
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
- Advanced audio: EQ, compression, noise reduction beyond `:duck`.
- Video effects/filters beyond transform and transitions.
- ML-based scene detection hook.
- Plugin distribution/package manager.
