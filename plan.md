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

## TUI preview

The terminal previews inline; what it cannot yet do is put the picture in a
window instead, which is only worth building for people who already have a
display.

### Step 1 - optional detached window (`--preview-window`)

The bare, undecorated, non-focusable window spec 15.5 describes, opened only
when asked for. A `winit` window in `davimci-cli` holding the same texture the
GUI path uploads, so no frontend gains a second video path.

- Off by default: it needs a display, which is what the terminal frontend is
  usually run without.
- The terminal keeps keyboard focus, and losing the window is a recoverable
  error that falls back to inline preview rather than ending the session.

Testing:
- The existing host-parity test extended: `Embedded`, `Detached` and inline
  must present identical pixels for the same frame.
- A test that closing the window mid-session leaves editing alive and switches
  preview back to the terminal.

---

## Milestones

Every milestone is met: the GUI edits video, plays it in sync, saves, exports
a multi-audio MKV, and the whole Lua surface - motions, text objects, keymaps,
hooks, presets, transition types - reaches a running editor. Overlays and
subtitles are editable through `:set` and exportable burned, sidecar or
embedded, and the optional TUI passes cross-frontend parity with an inline
preview - the detached window it can be given instead is the one thing still
outstanding, above.

---

## Deferred (tracked, not in v1)

- Zero-copy hardware-decode surface import into the `wgpu` presenter. The rest
  of spec 10.6's GPU preview concern is answered by the presenter as built.
- A custom subtitle layout engine in place of MLT's text producers (spec 10.6).
- Beat detection as a jump-point source.
- Advanced audio: EQ, compression, noise reduction beyond `:duck`.
- Video effects beyond transform and transitions.
- ML-based scene detection hook.
- Plugin distribution and package management.
- Backwards playback with audio; today `H` past zero is a stepped, silent
  scrub.
