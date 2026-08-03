# davimci - Implementation Plan

Companion to `spec.md`. `spec.md` says how davimci behaves; this says what is
left to build and how each piece is proved. Finished phases are struck from
here as they land - what they taught is recorded in `changes.md`. Loose ends
too small to plan live in `todo.md`.

No schedule or effort is implied by the ordering; items are ordered by
dependency only.

---

## Standing constraints

| Concern | Choice | Rationale |
|---|---|---|
| Core language | Rust | Memory safety around the MLT C API, strong enum modelling for modes and commands. |
| Lua runtime | `mlua` with vendored Lua 5.4 | System Lua on Arch is 5.5, which `mlua` does not support. Vendoring pins the version. |
| Render backend | `libmlt` via a hand-written `-sys` crate plus a safe wrapper | Per spec 10.1. |
| Video presentation | `winit` + `wgpu` textured quad, frames pulled from MLT, audio clock as master | MLT's own consumer owns its window and cannot be composited with our overlays. |
| Primary UI | One `egui`-on-`wgpu` window: chrome plus a custom-painted timeline | Keyboard-first is an input grammar, not a pixel backend. |
| Serialization | `serde`, versioned project format, binary analysis cache | Human-diffable projects, compact caches. |
| Errors | `thiserror` in libraries, `anyhow` only at the binary edge | Typed errors are required by the recovery policy. |
| License | GPL-3.0, dynamically linking LGPL-2.1 `libmlt` | Per spec 13. Never static-link MLT; never vendor `melt`. |

```
crates/
  davimci-core/      timeline model, clips, tracks, grouping, marks, registers
  davimci-cmd/       command objects, undo tree, macro recorder
  davimci-motion/    motions, text objects, jump points, predicate index
  davimci-analysis/  import/conform, waveform, silence, scene change, proxies
  davimci-backend/   RenderBackend trait
  davimci-mlt-sys/   raw FFI bindings
  davimci-mlt/       safe wrapper implementing RenderBackend
  davimci-lua/       Lua API surface, config loader, autocmds
  davimci-keys/      key sequence parser, mode FSM
  davimci-app/       frontend-agnostic view state and event loop
  davimci-present/   video path: pacing, letterboxing, composition
  davimci-gui/       primary frontend
  davimci-tui/       optional terminal frontend
  davimci-headless/  scriptable frontend for tests
  davimci-cli/       binary
```

Four error classes, each with a fixed policy:

| Class | Example | Policy |
|---|---|---|
| User error | Trim past a clip's handles, bad `:command` | Reject before mutating. Status-line message. Never enters the undo log. |
| Missing media | Source file moved between sessions | Project opens, clips flagged offline and rendered as a placeholder, editing allowed, export refuses. `:relink` fixes it. |
| Recoverable runtime | Decode failure on one frame, a Lua callback throws | Degrade locally, notify, keep editing. |
| Corruption | Failed invariant, deserialization failure | Flush the autosave log, report, exit cleanly. |

The architectural rules these rest on are in `AGENTS.md`.

---

## 1. TUI frontend (`davimci-tui`, `--features tui`)

Explicitly a nice-to-have. Ships only if it stays thin; cut without regret
otherwise.

Deliverables:
- `ratatui` timeline, ruler, status line and command line, rendered from the
  same `davimci-app` view state as the GUI.
- Preview through `davimci-present` in `Detached` mode, `:set preview off` for
  no-display sessions.
- Terminal key translation into `davimci-keys` tokens.
- Documented limitations: no in-video overlays, no properties panel, coarser
  timeline resolution.

Testing:
- Terminal snapshot tests at fixed sizes per mode.
- The cross-frontend parity test extended to three hosts: one scripted session
  through headless, GUI and TUI must produce an identical timeline snapshot
  and identical view state. A divergence is a frontend bug, never a core one.
- Degradation test: with preview disabled and no display, the TUI still starts
  and every edit works.

---

## 2. Integration and hardening

Deliverables:
- A scripted-session file format - keystrokes plus assertions - usable as both
  a test format and a debugging tool.
- Performance validation against spec 14: 1080p60 playback and editing smooth,
  split/ripple/undo instant on a few hundred clips, predicate motions never
  scanning. Coarse targets, measured before any optimisation.
- A documented default keymap generated from the keymap table, so it cannot
  drift from the code.

Testing:
- Full-workflow integration test mirroring spec 1: import a multi-track MKV,
  ripple-delete sections, mute and trim an audio track, add an overlay, add
  subtitles, export; assert `ffprobe` output and a golden timeline snapshot.
- Soak fuzz: random key sequences against a fixture project, asserting no
  panic, invariants hold, and undo returns to the initial state exactly.
- `criterion` benchmarks with regression thresholds for jump-point
  computation, ripple delete on a large timeline, predicate lookup, undo of a
  long log, and project load.
- A long editing session under ASan.

---

## Cross-cutting test strategy

| Layer | Primary technique |
|---|---|
| Timeline model | Property tests plus invariant assertions |
| Commands and undo | Apply/invert round-trip properties, serialization fuzz |
| Motions and objects | Table-driven landing-position tests |
| Key parsing | Golden key-string to command-sequence tests |
| Analysis | Generated fixture media with known ground truth |
| MLT wrapper | Sanitizer-backed refcount tests, golden XML projection |
| Lua API | Spec snippets as executable acceptance tests |
| Export | `ffprobe` assertions on real output |
| View state | Pure viewport and zoom unit and property tests |
| Presenter | Offscreen image-diff snapshots, synthetic-clock pacing tests |
| GUI | Draw-list snapshots plus input translation tests |
| TUI | Terminal snapshot tests plus cross-frontend parity |
| Whole app | Scripted headless sessions plus soak fuzzing |

Standing rules:

1. `davimci-core` and `davimci-cmd` have no backend and no I/O, and stay fully
   unit-testable in-process.
2. Every bug fix lands with a regression test naming the issue.
3. Default `cargo test` is fast; anything needing real decode or encode sits
   behind `--features slow-tests`.
4. Test media is generated, never committed.
5. CI runs the default suite, the slow suite, a sanitizer build, clippy and
   rustfmt, a Lua-config compatibility suite loading every example config in
   the docs, and a lavapipe job for presenter and GUI snapshots.
6. No frontend contains view logic. A fix needed in both `davimci-gui` and
   `davimci-tui` belongs in `davimci-app` or `davimci-present`; the parity test
   exists to catch this.
7. The GUI is the reference frontend. Headless and TUI are validated against
   it, not the reverse.

---

## Milestones remaining

M1 to M6 are met: the GUI edits video, plays it in sync, saves, exports a
multi-audio MKV, and the whole Lua surface - motions, text objects, keymaps,
hooks, presets, transition types - reaches a running editor. Overlays and
subtitles are editable through `:set` and exportable burned, sidecar or
embedded.

| M | Definition of done |
|---|---|
| M7 | Optional TUI behind `--features tui`, passing cross-frontend parity. Cut without regret if it is not thin. |
| M8 | Hardened: soak-tested, 1080p60 validated, documented default keymap. Crash recovery already restores the undo tree. |

M5 (audio operations) landed with M3. M7 is deliberately late: the TUI is a
convenience, and shipping it early would mean maintaining two frontends
through every core change.

---

## Deferred (tracked, not in v1)

- Zero-copy hardware-decode surface import into the `wgpu` presenter. The rest
  of spec 10.6's GPU preview concern is answered by the presenter as built.
- Terminal-inline preview (kitty/sixel) as a TUI fallback when no window can
  be opened. Escape-sequence throughput gives no frame-pacing guarantees, so
  it can never be the primary path.
- A custom subtitle layout engine in place of MLT's text producers (spec 10.6).
- Beat detection as a jump-point source.
- Advanced audio: EQ, compression, noise reduction beyond `:duck`.
- Video effects beyond transform and transitions.
- ML-based scene detection hook.
- Plugin distribution and package management.
- Backwards playback with audio; today `H` past zero is a stepped, silent
  scrub.
