# davimci - Implementation Plan

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
  davimci-core/      timeline model, clips, tracks, grouping, marks, registers
  davimci-cmd/       command objects (apply/invert), undo tree, macro recorder
  davimci-motion/    motions, text objects, jump points, predicate index
  davimci-analysis/  waveform/RMS, silence, scene change, proxy jobs, cache
  davimci-backend/   RenderBackend trait
  davimci-mlt-sys/   raw FFI bindings
  davimci-mlt/       safe wrapper implementing RenderBackend
  davimci-lua/       Lua API surface, config loader, autocmds
  davimci-keys/      key sequence parser (counts, operators, objects), mode FSM
  davimci-app/       frontend-agnostic app state: viewport, zoom, selection, msgs
  davimci-present/   winit+wgpu video surface: texture upload, frame pacing, sync
  davimci-gui/       primary frontend: present + egui chrome + painted timeline
  davimci-tui/       secondary frontend: ratatui timeline + detached present window
  davimci-headless/  scriptable frontend: no window, used by tests and CI
  davimci-cli/       binary, arg parsing, frontend selection, project open/save
```

The hard rule from spec §10.1: nothing outside `davimci-mlt` may reference MLT
types. `davimci-core` must compile and be fully testable with the backend absent.

---

## 0.1 Frontend Strategy (one product, three hosts)

The risk called out explicitly: **three frontends must not become three
implementations.** They are avoided by making the frontends thin.

Everything meaningful lives below the frontend line:

```
            davimci-app  (viewport, zoom, selection, status, pending input)
                 |  Frontend trait: present_frame / draw / poll_input
   +-------------+-------------+--------------------+
   |             |             |                    |
headless        gui           tui            (future frontends)
(no window)  present+egui   ratatui + present-only window
```

- `davimci-app` owns *all* view state - zoom level, scroll offset, which tracks
  are visible, selection highlighting, status-line content, message queue. A
  frontend asks it "what should be on screen" and draws it. No frontend
  computes layout semantics.
- `davimci-present` is the single video path. The GUI hosts it inside its main
  surface; the TUI hosts it in a bare window with no widgets and
  `with_decorations(false)` / focusable disabled, so the terminal keeps
  keyboard focus. **The TUI mode adds a window host, not a renderer.**
- Input always flows through `davimci-keys` into commands. A frontend translates
  platform key events into davimci key tokens and does nothing else with them.

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

## Phase 1 - Timeline Model Core (`davimci-core`)

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

## Phase 2 - Command Layer & Undo Tree (`davimci-cmd`)

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
replay is keystroke-shaped as in vim; `davimci-keys` (Phase 4) gives the tokens
meaning. History itself is not yet persisted - a reopened project starts a
fresh undo tree from the saved state.

---

## Phase 3 - Motions, Jump Points, Text Objects (`davimci-motion`)

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

## Phase 4 - Key Parser & Mode FSM (`davimci-keys`)

Deliverables:
- Input grammar: `[count] [register] operator [count] motion|textobject`,
  plus standalone commands, `g`-prefixed sequences, and `<Space>` leader
  sequences (spec §3.2.1).
- Transport bindings: `<Space><Space>` play/pause, `H`/`L` shuttle (no default stop key),
  `<Space>p` preview-and-return, `<Space>l` loop selection. Transport dispatches
  to the backend clock, **not** through the undo log - playback is not an edit.
- Transport policy per action (spec §3.2.1): a motion or an edit typed during
  playback interrupts the clock and commits the playhead before it runs; zoom,
  mode changes, and the transport family itself leave it running. Lua bindings
  opt in with `{ interrupt = true }` and can pause explicitly with
  `editor.interrupt_transport`.
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

Status: complete. The grammar (`davimci-keys::parser`) is a pure state machine
over `davimci-motion`'s `BuiltinMotion`/`TextObject` types and never touches a
`Timeline`, so golden key-string tests need no fixture. A separate `engine`
module gives the parsed `Action` meaning against a live `davimci_cmd::Session`,
which is the layer plan.md Phase 2 deferred ("`davimci-keys` gives the tokens
meaning"). Playhead motion and marks are intentionally outside the undo log -
`Session` gained narrow `set_playhead`/`set_mark` escape hatches for this,
since navigation was never meant to be a `Command`.

Known gaps, tracked against later phases rather than left silent: `i`/`a`/`r`
need the Phase 5 media picker; `gx`/`dax` wait on Phase 9f transitions;
`<`/`>` jump-point edge trims parse but are not wired to a command yet;
visual-mode text-object narrowing (typing `it`/`at` while a selection is
live, spec §6) is not implemented - operators in a `VISUAL*` mode act on the
whole selection instead.

---

## Phase 5 - Media Import, Conform & Analysis (`davimci-analysis`)

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
  `.davimci/cache/<content_hash>.analysis`, versioned and invalidated on bump.
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

Status: complete. The crate is split so that everything expressible as a pure
function of data is one - ffprobe JSON parsing, the fit rectangle, the conform
matrix, SRT parsing, silence detection, the predicate index, the proxy
threshold rule - and only `probe::FfprobeProber`, `decode`, `proxy::generate`
and `cache` touch the outside world. That is what keeps the default suite free
of decode/encode; the fixture-backed tests live in `tests/media.rs` behind
`--features slow-tests`.

Amendments made during implementation:

- Import had to become a real edit, so Phase 2's command set gained `AddTrack`
  and `RemoveTrack` and Phase 1 gained `add_track_with_id`/`remove_track`. An
  import is one `Sequence`; undoing it removes the tracks it created.
- A command cannot mint an id its own siblings need, so ids are pinned before
  the sequence is built (`Timeline::reserve_ids`, `Session::reserve_ids`).
- Re-conform is *not* self-inverse: rounding at one rate is not reversible at
  another, and a clip that rounds to zero frames has to be repaired rather
  than lost. `Reconform` therefore inverts to `RestoreConform`, which replays
  the exact prior geometry (`davimci_core::conform`). Undo of a rate change is
  byte-identical.
- Predicate queries index a threshold chosen at *query* time, which a sorted
  list cannot answer in log time. `index::MaxTree` is a max segment tree with
  a directional descent, so `]a` is O(log n) for any threshold and never
  scans.
- Spec §10.3's `prores_proxy` was not an ffmpeg encoder name; the slow tests
  caught it. The default is now `prores_ks` at profile 0, and spec.md says so.

Not yet wired: nothing calls this from a frontend, because there is no
frontend. `Predicate::Tagged` matches nothing until clip tags exist (Phase 7),
and analysis measures the source rather than the post-gain signal, so the
`invalidate`/`:analyze` path in Phase 9e is a hook without a caller.

---

## Phase 6 - Render Backend (`davimci-backend`, `davimci-mlt-sys`, `davimci-mlt`)

Deliverables:
- `RenderBackend` trait: `probe`, `seek`, `frame_at`, `preview_start/stop`,
  `next_preview_frame`, `audio_clock_position`, `render(job)`, `progress`.
- **Frame-pull preview, not MLT's video window.** Audio realtime output uses
  MLT's `sdl2_audio` / `rtaudio` consumer; video frames are pulled by us as
  RGBA buffers and handed to `davimci-present`. This is the Shotcut pattern and
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

Status: complete. The crate is layered by testability: `projection` turns a
`Timeline` into the shape the graph must have (pure data, no MLT), `xml`
serialises that shape for the golden tests, `patch` diffs two projections, and
only `ffi`/`backend` touch the C API. `MockBackend` lives in `davimci-backend`
and decodes nothing: a mock frame's colour is a pure function of its position,
so an upstream test asserts *which* frame it got from four bytes.

The incremental projection is real, not aspirational: a split patches one
playlist entry and inserts one, and a ripple delete removes one - both assert
that the rebuild counter did not move. A property test applies every generated
patch to the previous entry list and requires the result to equal a full
rebuild, which is what makes patching safe to prefer.

Amendments made during implementation:

- The projection reads track mute/solo and the media offline flag, and nothing
  could set them, so Phase 1 gained `set_track_muted`, `set_track_solo`, and
  `set_media_offline`. Solo turned out to need a defined meaning; spec §6.1
  now says it is exclusive by effect, so any solo silences every non-solo
  track and the backend resolves it at projection time.
- Pulling frames directly from a tractor bypasses the consumer that normally
  plants MLT's normalising filters, so `mlt_frame_get_image` returned native
  YUV at native size and ignored the requested dimensions - preview scaling
  would have been a lie. Producers now carry `avcolor_space`/`rescale`/`resize`
  themselves, and `FrameRef::rgba` verifies the *returned* format instead of
  trusting the requested one. Trusting it read past the end of a smaller YUV
  buffer; the slow tests caught it as a segfault.
- `mlt_events_listen` hands back an event the properties bag still owns, so the
  listener handle takes `mlt_event_inc_ref` before it can be closed. Without it
  stopping a preview double-freed.
- Refcount testing is the wrapper unit tests (`clone_ref` is balanced by drop,
  64 create/clone/drop cycles do not grow the count, a playlist planted in a
  tractor outlives its wrapper) - they assert on MLT's own `ref_count()`
  directly rather than relying on a sanitizer. `just sanitize` now runs
  (nightly + `rust-src` installed via `rustup`) and is green with a narrow,
  documented LSAN suppression file
  (`crates/davimci-mlt/lsan-suppressions.txt`) for MLT's own one-time
  module-init state and its internal blank-producer path, neither of which
  davimci constructs or holds a handle to. LeakSanitizer's stack scan is
  conservative and can miss a real leak, so a clean run is evidence for the
  wrapper, not proof; the `ref_count()` assertions remain the primary
  guarantee.

Not yet wired: nothing calls this from a frontend, because there is no
frontend. Transitions are absent until Phase 9f, so the projection plants no
MLT transitions yet, and the export preset registry that would exercise
`RenderSettings` properly arrives in Phase 8b.

---

## Phase 7 - Lua Config & Plugin API (`davimci-lua`)

Deliverables:
- Config loader honoring `~/.config/davimci/init.lua`, `keymaps.lua`,
  `motions/`, `presets/`, `plugin/`, and opt-in `.davimci.lua` project-local
  overrides with an explicit trust prompt.
- Modules: `davimci.keymap`, `davimci.motions`, `davimci.textobject`, `davimci.export`,
  `davimci.timeline`, `davimci.media`, `davimci.autocmd`, `davimci.editor`.
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
- Sandbox tests: project-local `.davimci.lua` is not executed without trust;
  untrusted config cannot reach `os.execute`/`io` by default.
- Panic-safety test: user callback that throws leaves the editor usable and
  the timeline unmodified.

Status: complete. The crate's shape follows one rule - **Lua asks, it never
writes.** A `davimci.*` call either registers something or appends a `Request`;
the host runs each request through `davimci_cmd::Session`, so a plugin edit is
an ordinary undo-tree entry and the single-write-path rule holds at the
plugin boundary. That is also what keeps the crate testable: `Runtime` needs
no timeline, no backend, and no window, and the spec §9 snippets run verbatim
as the acceptance suite.

Amendments made during implementation:

- A Lua function right-hand side (spec §9.2) had nothing to resolve to, since
  `davimci-keys` must not depend on `davimci-lua`. `Action::Plugin(u32)` and
  `Outcome::Plugin(u32)` carry an opaque callback id instead: the engine
  reports it, the host invokes it, and `Engine::execute_action` (new, public)
  runs whatever edits come back. Spec §9.9 now documents the request model
  and the `editor.*` command set that a string right-hand side may name.
- A registered motion cannot be handed a live `Timeline` without becoming a
  second write path, so it receives a `MotionEnv` snapshot - playhead,
  focused track, clip bounds, analysis samples. `find_next` over an
  unanalysed track reports `Pending` rather than `NoMatch`, matching
  `davimci_motion::Answer`; a Lua motion cannot accidentally be more confident
  than the analysis it queries.
- Cancellation needed defining, not just implementing: a `BeforeExport`
  handler refuses by returning `false` *or* by throwing. Throwing also
  disables the handler (Phase 0 recoverable policy); a `false` return is a
  deliberate veto and leaves it in place. Spec §9.8 says so now.
- Trust is not a binary: spec §9.7 said "opt-in" without saying what a
  trusted file may do. An untrusted `.davimci.lua` is never read, and a trusted
  one still runs sandboxed (no `os`, `io`, `load`, `dofile`, and a `require`
  that resolves `davimci.*` only). Spec §9.7 now spells this out.
- Export presets validate at definition, and codec names map to ffmpeg
  encoders here rather than in the preset, so §10.3's "never a marketing
  name" rule cannot be broken by a config.

Not yet wired: no frontend calls any of this, so `Runtime::take_requests` has
no production caller; `Request::Import`/`Analyze` wait on Phase 8/9e, and
text objects registered from Lua are resolvable but not yet reachable from
the key grammar, which still resolves only the built-in `ic`/`ac`/`it`/`at`/
`is`. Keymap overrides are applied for `NORMAL` only, because
`davimci_keys::Keymap` is a single table for every mode.

---

## Phase 8 - Project Lifecycle (`davimci-cli`)

Deliverables (spec §12):
- `:w`, `:q`, `:q!`, `:wq`, `:e`, `:new`, `:ls`, `:bn`/`:bp`/`:b <n>`.
- Multiple open timelines with global registers and marks shared across them.
- Continuous autosave of the command log to `.davimci/autosave/`, never touching
  the project file; crash recovery prompt on next open.
- `:relink` for offline media (Phase 0 policy).

Testing:
- Save/load round-trip: byte-identical timeline state after reload.
- Dirty-state tests: `:q` refuses with unsaved changes, `:q!` discards.
- Cross-timeline yank/paste test.
- Crash-recovery test: kill the process mid-session, reopen, assert the log
  replays to the exact pre-kill state.
- Format-migration test: load a project written by an older schema version.

Status: complete. `davimci-cli` is the first crate allowed to touch the
filesystem, and it is split so that only the parts that must: `excmd::parse`
is a pure function from a `:` line to an `ExCommand` and is table-tested
against the spec §12 vocabulary, while `Workspace` is the only thing that
reads or writes. Every edit it performs is still an `EditCommand` - `:relink`
and `:e <media>` are ordinary undo-tree entries - so the single-write-path
rule survives contact with I/O.

Amendments made during implementation:

- `:relink` had no command to run, so Phase 2 gained `EditCommand::Relink`
  and Phase 1 gained `Timeline::set_media_source`. The offline flag is decided
  by the CLI, which is the only layer that may ask whether a file exists, and
  passed *in* to the command; `davimci-core` never stats a path. The inverse
  restores both the old path and the old offline flag, so a mistaken relink
  is one `u` away. Spec §12 now documents both argument forms.
- Spec §12 called registers and marks "global" but the model stores both on a
  `Timeline`. `Workspace` implements global by syncing on every buffer
  switch, and `Session::set_register` joins `set_mark` as a non-command
  escape hatch, on the same reasoning: a register is bookkeeping, not
  timeline content, and vim does not put either in the undo log. A mark's
  focused track is dropped when it crosses into a timeline that has no such
  track; spec §12 says so now.
- Dirty state is `history().current() != saved_at`, not a flag, so undoing
  back to the saved state is clean again. This fell out of the undo tree and
  is worth more than a boolean: it makes `:q` refuse exactly when the file
  and the timeline actually differ.
- Autosave stores the log, not the state, and syncs after every edit. Undo
  shortens the log rather than extending it, so the writer appends only when
  the current log is an extension of what is on disk and rewrites otherwise.
  Each line carries the id cursor, because replaying pinned commands mints no
  ids and a recovered session would otherwise reissue ids the crashed one had
  already used.

Not yet wired: there is still no frontend, so the binary is a thin driver -
it opens a project, answers the recovery prompt, and runs `:` commands given
with `-c`. `:analyze` is listed in spec §12 but belongs to Phase 9e and is
not accepted yet, and undo history is still not persisted across a save
(Phase 2's standing gap): a recovered log replays into a fresh tree.

---

## Phase 8b - Export (`davimci-export` within `davimci-cli`)

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

## Phase 9a - View State (`davimci-app`)

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

Status: complete. The crate holds every decision a frontend would otherwise
make for itself: zoom, scroll-follow, ruler ticks, the mode line, what an
`Outcome` says in the status line, and the meaning of a key. A frontend polls
events, reports its size, and draws a `ViewState` - `davimci-app` never sees a
window and does no I/O, so all of it is unit-testable with no display.

Amendments made during implementation:

- A "column" is deliberately unitless: a GUI pixel or a TUI cell, whichever
  the frontend measures in. That is what lets one `Viewport` serve both, and
  it is why `Surface` carries `columns`/`rows` rather than pixels.
- `davimci-keys` reports `:` as an ordinary mode change to `COMMAND`, not as
  `Outcome::EnterCommandMode`, so the app watches `ModeChanged` and hands the
  keyboard over on that. Owning the `:` line stays a frontend job; deciding
  what the line *means* stays the host's.
- Spec §15 is new: the status-line format for every mode, the scroll-follow
  and zoom-anchoring rules, and the zoom keys `zi`/`zo`/`z0` (spec §11).
  Zoom is view state, so `davimci-keys` only reports `Outcome::Zoom` and
  `App::zoom_*` applies it - the same entry point a pointer wheel or a menu
  uses, and nothing zoom-related reaches the undo log.
- `Host` is the seam for the three things the editor core deliberately does
  not own - `:` commands (`davimci-cli`), transport (the backend clock), and
  Lua callbacks (`davimci-lua`) - so `davimci-app` depends on none of them.
- `davimci-headless` was filled in here rather than in 9d: it is a `Frontend`
  that records view dumps, which is what the parity test compares.

---

## Phase 9b - Video Presenter (`davimci-present`)

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

Status: pacing, fitting, composition and overlays complete; the windowed host
is not written yet.

What exists is the whole decision layer: `Pacer` (drop-late / repeat-on-starve
with counters, tested against `MockBackend` for in-step, fast, starved and
jittery sources), `letterbox` (integral, centred, float-free, matrix-tested),
`Presenter` (nearest-neighbour composition into an RGBA surface plus an
overlay model), and `HeadlessPresenter`. The host-parity test is real and
passes: `Embedded` and `Detached` produce byte-identical video pixels, and the
only permitted difference is that `Detached` describes no overlay.

Amendments made during implementation:

- Composition is software and integral rather than `wgpu`-first. That is what
  makes the parity and pacing tests byte-exact assertions instead of
  tolerance-based image diffs, and it fixes the pixels a future GPU upload
  path must *reproduce* rather than redefine. `winit`/`wgpu` surface creation
  is therefore deferred to the windowed shell (9c), not to a second video
  path.
- The presenter describes overlays, it does not rasterise text: a timecode is
  a string and safe areas are rectangles, drawn by the host's own text stack.
  Rasterising here would give the GUI and the TUI two different-looking
  timecodes for the same frame. Spec §15.5 says so now, along with the
  drop/repeat policy and the drop-frame-free timecode format.
- Scale selection is one-directional (`auto_scale` never decodes below what is
  drawn), so a small window is cheap without being soft.

Not yet wired: no window is created, and `PresentError::Pull` has no
production caller until a frontend drives playback.

---

## Phase 9c - GUI Frontend (`davimci-gui`) - primary

Deliverables:
- Single window: video quad (`davimci-present` embedded) + custom-painted
  timeline (tracks, clips, ruler with jump-point ticks, playhead, selection)
  + `egui` chrome.
- Status line, command line (`:`) with history and completion.
- Media picker for `i`/`a`/`r`; text-edit INSERT mode for subtitle clips;
  clip properties panel (spec §8 transforms).
- Key event translation into `davimci-keys` tokens; nothing else.

Testing:
- Image-diff snapshot tests of the full window at fixed sizes, covering each
  mode, selection kind, and panel state.
- Layout tests at extreme sizes (very short, very narrow, more tracks than
  fit) asserting no panic and a sane viewport.
- Input-translation tests: synthetic `winit` events produce the expected key
  tokens, including modifiers and `<Left>`/`<Right>`.
- Golden view-state reuse: renders are driven by the Phase 9a fixtures, so a
  view-state regression fails in both `davimci-app` and here.

Status: the frontend's decision layer is complete and tested; the `winit` +
`wgpu` + `egui` window that rasterises it is not written yet.

Implemented: `Layout` (video pane, ruler, track lanes and headers, status and
command lines, derived from window size with a documented priority order),
`paint` (a `ViewState` plus a layout to a `DrawList` of typed rectangles and
text runs - no colours, so a theme cannot move anything), key translation,
the `:` line with history and longest-common-prefix completion, the media
picker for `i`/`a`/`r`, and INSERT-mode subtitle editing. `Gui` implements
`davimci_app::Frontend` and routes modal input, so a modal owns the keyboard
and the key grammar never sees those keystrokes.

Amendments made during implementation:

- The raw key model is davimci's own (`RawKey` + `Modifiers`), not `winit`'s,
  so translation is testable with no window and the same table can serve a
  terminal adapter. Shells fill in a `RawKey`; they may not decide what a key
  means.
- Painting is split from windowing because that is where a rendering
  regression actually lives. The draw-list summary is the snapshot, and the
  golden view states from 9a drive it, so a view-state change fails in
  `davimci-app` and here - the reuse plan.md asked for, without a GPU in the
  test suite.
- Modal behaviour needed defining rather than inventing: spec §15.3/§15.4 now
  say that Esc or backspacing over the `:` cancels the line, that Tab
  completes to the longest common prefix, how the picker filters and wraps,
  and that an INSERT-mode edit ending equal to the original text commits
  nothing (so it never enters the undo log).
- A two-way parity test already runs (GUI vs headless: same script, identical
  view dumps); it becomes the three-way test of 9d when the TUI lands.

### The shell

Status: complete. `davimci-gui`'s `egui_shell` is the one module in the
project that knows what a colour or a font is; it rasterises the `DrawList`
that `layout::paint` already computed and uploads the RGBA surface that
`davimci-present` already composited. It sits behind a `window` feature, so
with it off the crate is pure and needs no display - which is how the layout,
painting and input tests still run headless.

`davimci_cli::Window` is the eframe application, and it lives in the binary
for the same reason `Editor` does: it holds a frontend and a render backend at
once, and no frontend may reference MLT. eframe 0.35 splits `App::logic` from
`App::ui`, which lines up exactly with the split this codebase already had -
decisions in `logic`, pixels in `ui`.

Running `davimci <media>` with no `-c`/`-k` now opens the editor: a real
window, a real MLT decode, the timeline painted from the view state, and keys
going through the grammar. Verified against a generated 1080p60 fixture.

Amendments made during implementation:

- Printable keys are taken from egui's `Event::Text` (already shifted by the
  platform layout) and named keys and Control chords from `Event::Key`, since
  `Text` is emitted for neither. Whitespace text is dropped so `Space` cannot
  arrive twice - it is a leader (spec §3.2.1) and a double press would fire
  the wrong binding.
- The presenter's surface is kept equal to the video pane, so
  `davimci-present` letterboxes into exactly the rectangle that will be
  drawn and the shell never scales an image twice.

Defect found by looking at the window: a composed frame is sized to the
surface it was composed for, and nothing recomposed it when the pane resized,
so the picture kept its startup size in the corner of the pane.
`Editor::refresh_preview` now recomposes on a size change, with a regression
test (`resizing_the_video_pane_recomposes_the_frame_at_the_new_size`).

### Phase 8b - export

Status: implemented, with one gap named below.

The MLT side already existed; what was missing was everything around it.
`davimci-backend::preset` holds the preset registry, and it is pure data - it
maps a *codec* name to an ffmpeg encoder (spec §10.3) and refuses an
impossible container/codec pairing when the preset is **defined**, not after
a long render (spec §9.5). `davimci-cli::export` drives one, turning backend
progress into job updates and a final status line.

`:export <path> [--preset <name>]`, `:render <preset>`, `:presets` and
`:cancel` are the commands. They never reach `Workspace`, which has no
backend; `Editor` intercepts them, because it is the only thing holding one.
For the same reason a `-c` script containing an export command now runs
through a real editor rather than a bare workspace, which is what makes
batch export from the command line work.

Amendments made during implementation:

- `:cancel` and `:presets` were added to spec §7's export list; neither was
  named there and both are needed to use the feature.
- An export is a background job, so the editor stays live while one runs.
  Progress is polled on the tick, through a new `Host::jobs` seam, since a
  host runs jobs and the app only displays them.
- Progress never reports 100% before the backend says the render finished; a
  status line that reads "100%" for thirty seconds is a lie about a file that
  does not exist yet.

**Known gap, and M3 is not met until it closes.** Exporting a multi-audio
MKV currently mixes every audio track down to one stream. MLT's avformat
consumer can write up to 8 audio streams via `meta.map.audio.{N}.channels` /
`.start`, but the tractor mixes the tracks before the consumer sees them, so
there are no per-track channels left to map. The fix is per-track channel
routing when the graph is built (`davimci-mlt`), not an export setting. The
test that proves it is written, asserts the real requirement, and is
`#[ignore]`d with that explanation rather than weakened.

### Phase 5 leftover - the media picker opener

Status: complete. `i`/`a`/`r` no longer report `NotImplemented`.

The chain is: `davimci-keys` returns `Outcome::PickMedia(MediaIntent)` (the
grammar has no filesystem, so it only says what a file would be *for*), the
app answers `Response::OpenPicker`, the frontend opens its picker and replies
with `Event::MediaChosen` or `Event::PickerCancelled`, and the host - which
has the prober - imports at the position the intent implies. Directory
listing lives in `davimci-app::browse` rather than in a frontend, because the
GUI and the TUI must show the same files in the same order or the parity test
is meaningless.

Defect found on first real use: pressing `i` looked like a freeze. The picker
opened and correctly took the keyboard, but nothing painted it - `paint` knew
only about the view state, and the picker is the shell's own state. An
invisible modal that swallows every key is indistinguishable from a hang.
`Chrome` now carries a `PickerView`, `layout::paint` draws it, and a
regression test asserts that an open picker reaches the draw list.

The same fix exposed a second one: the video texture is uploaded separately
and was drawn *after* the draw list, so the panel appeared behind the
picture. Modal ops are now identified (`Paint::is_modal`) and drawn after the
video, which is the only ordering that keeps an overlay on top of a texture
the painter does not own.

Amendments made during implementation:

- `ImportOptions` gained `placement` (insert vs overwrite) and `target`.
  Imported media ripples rather than overwriting, so picking a file never
  destroys work; `target` puts it on the track the playhead is on, since
  landing it on a fresh track would ignore where the user was looking.
- `r` is refused *before* the picker opens when no clip is under the
  playhead: opening a file browser for an edit that cannot land is a worse
  error than the message.
- All three intents are one command, so a single `u` undoes a whole import,
  including the ripple delete that `r` needs.

Defect found on first real use: opening a file whose name contains spaces
failed with a `:e <path>` usage error. The binary stringified an argv path
into a `:` line, and the parser split it on whitespace. Two fixes, since
there were two bugs: argv paths now go straight to `ExCommand::Edit` without
a round trip through the parser, and a single-path command's argument is the
rest of the line (spec §12), because media filenames contain spaces
constantly. Regression tests cover both spellings.

Not yet wired: clicks are translated to columns but not to a seek, and the
picker/subtitle modals have no production opener - `i`/`a`/`r` still report
`NotImplemented` from `davimci-keys`.

### Wiring and transport (the glue)

Status: complete. `davimci_cli::Editor` is the only type that holds a
workspace, a `RenderBackend`, a `Presenter` and the transport at once, and it
lives in the binary crate because no frontend may reference MLT (spec §10.1).
It implements `davimci_app::Host`, so the app drives it without knowing any
of that exists. `davimci_cli::Transport` implements `<Space><Space>`, `J`/`K`/
`L` and `<Space>p`.

`davimci <media> -k "<keys>" --ticks <n>` now runs the whole editor - key
grammar, command layer, MLT backend, presenter, transport - with
`HeadlessFrontend` in the window's place. This is verified against real media:
importing a 1080p60 file, splitting it, and playing it pulls real frames
through MLT into the presenter.

Amendments made during implementation:

- `Host` gained `tick`, `timeline_changed` and `playhead_moved`. Reporting
  them from one place in `App::apply_outcome` is what stops a host from
  missing an edit by handling only some outcomes; `tick` takes the session
  because playback *moves the playhead*, which is navigation on the same
  footing as a motion and still never an edit.
- `App::replace_session` and `Engine::reset` were needed for `:e`/`:bn`: a
  viewport column, a visual selection and a half-typed sequence all mean
  something only in the timeline they were made in, so they are reset rather
  than carried across. Registers survive, since spec §12 makes them global.
- Session ownership had to be decided rather than duplicated. `App` owns the
  live session; `Workspace` owns the buffers. The live one is pushed in
  before a `:` command and pulled back after
  (`set_current_session`/`current_session`), so `:w` writes what is on screen
  and `:bn` hands back a different timeline.
- Shuttle is a stepped scrub, not varispeed: `RenderBackend` has no rate
  control, so `J`/`L` stop audio and step the playhead, doubling to a capped
  rate and slowing through 1x before reversing. Real varispeed needs a trait
  method and is deferred rather than faked.
- `TransportCmd::LoopSelection` is refused with a sentence rather than
  silently ignored: looping needs the visual selection, which lives in the
  key engine and is not on the `Host` seam yet.

Defect found and fixed by running it: `Transport::tick` composed a frame and
`Editor::tick` composed another, so every tick pulled twice and the pacing
counters read roughly double. The presentation is now returned from the tick
that made it, with a regression test (`a_tick_presents_exactly_once`) naming
the bug.

---

## Phase 9d - TUI Frontend (`davimci-tui`) - optional, `--features tui`

Explicitly a stepping stone and a nice-to-have. Ships only if it stays thin.

Deliverables:
- `ratatui` timeline, ruler with tick marks, status line, command line -
  rendered from the same `davimci-app` view state as the GUI.
- Preview via `davimci-present` in `Detached` mode; `:set preview off` for
  no-display sessions.
- Terminal key translation into `davimci-keys` tokens.
- Documented limitations: no in-video overlays, no properties panel (falls
  back to command mode `:set clip.*`), coarser timeline resolution.

Testing:
- Terminal snapshot tests (`ratatui` test backend) at fixed sizes per mode.
- **Cross-frontend parity test:** one scripted session driven through headless,
  GUI, and TUI must produce an identical final timeline snapshot and identical
  `davimci-app` view state. This is the test that keeps three hosts from becoming
  three products; any divergence is a bug in the frontend, never in core.
- Degradation test: with preview disabled and no display available, the TUI
  still starts and all editing works.

---

## Phase 9e - Audio Operations (`davimci-core` + `davimci-mlt`)

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

## Phase 9f - Transitions (`davimci-core` + `davimci-mlt`)

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
1. `davimci-core` and `davimci-cmd` have no backend or I/O dependency and must
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
   `davimci-gui` and `davimci-tui`, it belongs in `davimci-app` or `davimci-present`
   instead - the cross-frontend parity test exists to catch this.
7. The GUI is the reference frontend. Headless and TUI are validated against
   it, not the reverse.

---

## Milestones

| M | Definition of done |
|---|---|
| M1 | Headless: load a fixture timeline, move playhead, split, ripple delete, undo - all via keys, verified by snapshot tests. No window code exists yet. |
| M2 | Import a multi-track MKV; frames pull from MLT into `davimci-present` and play in sync with audio in a bare window. No editing UI - proves the video path. |
| M3 | GUI: timeline + video in one window, playback and shuttle, scrub with jump points, trim, full cut workflow, save/load, export a multi-audio MKV. **This is the first genuinely usable build.** |
| M4 | Lua config fully wired: custom motions, text objects, keymaps, hooks, export presets. |
| M5 | Audio operations: mute, solo, gain, fades, waveforms - completing workflow step 3. |
| M6 | Overlays, subtitle tracks, and transitions editable and exportable. |
| M7 | Optional TUI frontend behind `--features tui`, passing cross-frontend parity. Cut without regret if it is not thin. |
| M8 | Hardened: soak-tested, 1080p60 validated, crash recovery, documented default keymap. |

The ordering rule: **nothing before M3 is a product.** M7 is deliberately
last-but-one - the TUI is a convenience, and shipping it early would mean
maintaining two frontends through every core change.
Correspondingly, `davimci-app` and `davimci-present` are built with two hosts in
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
