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

## 1. Wire the Lua config into the binary

The largest remaining gap, and the reason it is first: `davimci-lua` is
complete and tested against the spec's own snippets, but no other crate
depends on it, so no user config has ever been loaded by the editor. Every
later item is easier to expose once this seam exists.

Deliverables:
- `davimci-cli` loads `~/.config/davimci/` at startup and reports each notice
  from `Runtime::load_config` on the status line rather than failing.
- User keymaps reach `davimci-keys` before the default table is consulted;
  a `Plugin(u32)` outcome routes back through `Runtime::invoke`.
- `Runtime::take_requests` is drained on each tick and every request runs
  through `Session`, so a plugin edit is one undoable command like any other.
- Lua-registered motions, text objects and export presets are visible to the
  engine and to `:export`.
- Events dispatch for the v1 list, with `BeforeExport` able to cancel.
- Project-local `.davimci.lua` trust prompt goes through the app's modal path.

Testing:
- A headless session with a fixture config: a mapped key produces the edit
  the config asked for, and `u` undoes it in one step.
- A throwing callback disables itself, puts one notice on the status line,
  and leaves the session editable.
- The Lua-defined export preset reaches a real `:export`.

---

## 2. The `:set` family

Spec 8, 6.2, 7.1 and 15.5 all reach for `:set`, and none of them can be
finished without it. It is one command with a typed property registry, not
four special cases.

Deliverables:
- `:set clip.<prop> <value>` for transform (position, scale, opacity), gain,
  and fades - overlay editing in spec 8 is exactly this.
- `:set transition.duration <frames>` and `:set transition.type <name>`,
  replacing the re-run-`:transition` workaround.
- `:set timeline.fps <rate>` on the existing exactly-invertible re-conform.
- `:set preview off` for no-display sessions, needed by the TUI.
- Each setter is one `EditCommand`; view-only settings do not enter the log.
- Unknown property or out-of-range value is a user error, rejected with a
  sentence naming the property.

Testing:
- Table-driven: property, input, expected model change, expected inverse.
- A rejected `:set` leaves the timeline byte-identical.
- Golden MLT XML for a transform set through `:set` matching one set in-model.

---

## 3. Transport loop

Deliverables:
- `<Space>l` loops the live selection, which already rides the `Host` seam.
- Loop state belongs to the transport, not the undo log, and survives a seek
  inside the loop range.
- Playback stops at the loop end and resumes at its start without a reseek
  glitch, reusing the still cache.

Testing:
- Scripted headless session: set a selection, loop, tick past the end, assert
  the playhead wrapped rather than stopped.
- Clearing the selection while looping ends the loop with a message.

---

## 4. Remaining key grammar

Two actions parse today but reach no command.

Deliverables:
- `<`/`>` jump-point edge trims map onto the existing ripple-trim command,
  with the jump-point set deciding the edge.
- Typing `it`/`at` while a `VISUAL*` selection is live narrows it to a track
  rather than being ignored (spec 6).

Testing:
- Golden key strings for both, and a landing-position table for `<`/`>` at a
  clip boundary, at timeline start, and with a count prefix.

---

## 5. `:analyze`

The analyser re-runs itself when a track's audible signature changes, which
covers the reason the command exists, but spec 12 lists it and it is not
accepted.

Deliverables:
- `:analyze` drops the current project's envelopes and re-queues every audio
  source, reporting progress like any other background job.

Testing:
- Assert predicate motions report `Pending` while the job runs and answer
  correctly after it finishes.

---

## 6. Undo history across crash recovery

A saved project carries its undo tree (format v2). A recovered autosave does
not: the autosave log is a flat list, so recovery replays into a fresh tree.

Deliverables:
- Autosave records the tree edge each command was applied at, so recovery
  rebuilds the tree rather than a line.
- Recovery is still tolerant of a truncated final record.

Testing:
- Branch the undo tree, kill the process, recover, and assert `g-`/`g+`
  traverse the same branches as before the crash.
- A truncated autosave recovers everything up to the last complete record.

---

## 7. Subtitle export selection

`SubtitleSelection` is parsed and validated in presets, but the renderer only
ever burns text in. Spec 8 asks for sidecar and embedded too.

Deliverables:
- `burned` keeps today's behaviour; `sidecar` writes SRT next to the output;
  `embedded` muxes a subtitle stream where the container allows it.
- Container/codec validation already rejects the impossible pairings at preset
  definition time; the renderer trusts that and does not re-decide.

Testing:
- `ffprobe` the output of each mode: stream counts for `embedded`, absence
  plus a sidecar file for `sidecar`, and a pixel diff proving burn-in.

---

## 8. Lua-registered transition types

Deliverables:
- A `RenderBackend` method exposing the transition registry so `davimci-lua`
  can register a type without knowing MLT exists.
- `transitions::spec` is the seam; an unknown name keeps degrading to a
  dissolve so a project still opens in a build without the plugin.

Testing:
- Register a type from a fixture config, plant it, and assert the projected
  MLT XML names the right service.
- Open a project using an unregistered type and assert it degrades with a
  notice rather than failing the render.

---

## 9. TUI frontend (`davimci-tui`, `--features tui`)

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

## 10. Integration and hardening

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

M1 to M3 are met: the GUI edits video, plays it in sync, saves, and exports a
multi-audio MKV. That was the first usable build.

| M | Definition of done |
|---|---|
| M4 | Lua config fully wired: custom motions, text objects, keymaps, hooks, export presets, all reaching a running editor. |
| M6 | Overlays and subtitle tracks editable through `:set` and exportable burned, sidecar or embedded; transitions extensible from Lua. |
| M7 | Optional TUI behind `--features tui`, passing cross-frontend parity. Cut without regret if it is not thin. |
| M8 | Hardened: soak-tested, 1080p60 validated, crash recovery restoring the undo tree, documented default keymap. |

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
