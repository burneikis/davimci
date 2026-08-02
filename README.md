# davimci

A keyboard-first, modal video editor. Vim motions, verbs, and modes for
cutting footage, trimming audio, compositing overlays, and adding subtitles.
Configured like Neovim: `~/.config/davimci/init.lua`, a Lua scripting API,
remappable keys, and hookable events.

- [`spec.md`](spec.md) - what it is and how it behaves
- [`plan.md`](plan.md) - how it gets built, and how it gets tested

## Status

<!-- Keep this current. It must never claim more than the code does. -->

**Phase 8 complete - the project lifecycle.** Phase 8b (export presets and
`:render`) is next. Workspace builds; `just test` and `just lint` are green,
and `just fixtures && just test-slow` passes against generated media, now
including real decode, preview, and export through MLT.

The binary now does something: `davimci <project>` opens a project, offers
crash recovery if an autosave survived, and runs `:` commands passed with
`-c`. There is still no frontend (see plan.md milestone M1), so editing is
not yet interactive. The backend can project a timeline, seek frame-exactly,
pull frames, play audio, and encode a file, and a config file can bind keys,
define motions and export presets, and hook events - but only a test drives
any of it.

Lua 5.4 is vendored and built from source, so no system Lua is needed.

`libmlt` (>= 7) is now a build prerequisite: `just check-env` verifies it, and
it is linked dynamically, since davimci is GPL-3.0 over LGPL-2.1 MLT.

| Area | State |
|---|---|
| Error model and recovery policy (Phase 0) | implemented, tested |
| Frame-exact time, rational fps, conform math | implemented, tested |
| Timeline model: clips, tracks, grouping, marks, registers | implemented, tested |
| Edit primitives: split, lift, ripple delete, yank/paste, insert, overwrite, move | implemented, tested |
| Trim family: ripple trim, roll, slip, slide | implemented, tested |
| Commands: one serializable `EditCommand` per edit, apply returns its inverse | implemented, tested |
| Undo tree: `u`, `Ctrl-r`, `g-`/`g+`, `:undolist`, snapshot drift guard | implemented, tested |
| Repeat register (`.`) and macro record/replay buffers (`q`/`@`) | implemented, tested |
| Project format: snapshot + command log, versioned with a migration hook | implemented, tested |
| Motions: frame, jump point, clip, marker, `%`, marks, track focus | implemented, tested |
| Jump-point engine: zoom-aware, cached, monotonic in zoom | implemented, tested |
| Text objects `ic`/`ac`/`it`/`at`/`is` with track scope | implemented, tested |
| Predicate motions (`PredicateIndex` trait) | implemented, tested |
| Key grammar: counts, registers, operators, objects, `g`-prefixed and `<Space>`-leader sequences | implemented, tested |
| Mode FSM: NORMAL/VISUAL/VISUAL-LINE/VISUAL-BLOCK/INSERT/COMMAND, strict transitions | implemented, tested |
| Keymap table: defaults + user overrides, longest match wins, ambiguity/timeout handling | implemented, tested |
| Engine: grammar -> `Session` commands for split/ripple-delete/lift/yank/paste/trim family/gain/fades/undo/redo/repeat/macros | implemented, tested |
| Probe: ffprobe JSON to streams, exact rational rates | implemented, tested |
| Conform: framerate retime, letterbox/crop fit, audio resample flag | implemented, tested |
| Import: one track per audio/subtitle stream, SRT cues to text clips, one undoable command | implemented, tested |
| Re-conform: `timeline.fps` change with clips present, exactly invertible | implemented, tested |
| Analysis: peak/RMS at a 10 ms hop, silence spans, scene changes | implemented, tested |
| Predicate index: O(log n) peak/silence/scene lookup, `Pending` while analysing | implemented, tested |
| Analysis cache: `.davimci/cache/<hash>.analysis`, versioned, corruption-tolerant | implemented, tested |
| Background jobs: progress, cancellation, cancel-on-close | implemented, tested |
| Proxies: threshold rule, frame-exact spec, `BeforeExport` original-source guard | implemented, tested |
| `RenderBackend` trait: probe, seek, frame pull, preview, render, progress | implemented, tested |
| `MockBackend`: deterministic frames, no decode, for every upstream test | implemented, tested |
| MLT FFI: hand-written bindings, RAII wrappers, refcount-balanced `clone_ref` | implemented, tested |
| Timeline -> tractor/playlist projection, with golden MLT XML | implemented, tested |
| Incremental projection: split/ripple patch playlists instead of rebuilding | implemented, tested |
| Lua config loader: `init.lua`, `keymaps.lua`, `motions/`, `presets/`, `plugin/` | implemented, tested |
| Lua modules: `keymap`, `motions`, `textobject`, `export`, `timeline`, `media`, `autocmd`, `editor` | implemented, tested |
| Lua edits go through the command layer as requests, never a second write path | implemented, tested |
| Event dispatch for the v1 event list, with `BeforeExport` cancellation | implemented, tested |
| Error isolation: a throwing callback is disabled for the session, editing continues | implemented, tested |
| Project-local `.davimci.lua`: explicit trust, then sandboxed (no `os`/`io`) | implemented, tested |
| Export presets from Lua: codec/container validation, ffmpeg encoder mapping | implemented, tested |
| Frame pull: RGBA buffers to the presenter, MLT never owns a window | implemented, tested (slow) |
| Preview scaling: half/quarter requested at decode, not scaled afterwards | implemented, tested (slow) |
| Preview: realtime audio consumer as master clock, frames lifted from it | implemented, tested (slow) |
| Export: `avformat` consumer, polled progress, cancellation | implemented, tested (slow) |
| Mute/solo and offline-media flags on the model, honoured by the projection | implemented, tested |
| Project lifecycle: `:w`, `:q`/`:q!`, `:wq`/`:x`, `:e`, `:new`, `:ls`, `:bn`/`:bp`/`:b` | implemented, tested |
| Multiple open timelines with global registers and marks | implemented, tested |
| Autosave of the command log to `.davimci/autosave/`, crash recovery on reopen | implemented, tested |
| `:relink` for offline media, as one undoable command | implemented, tested |
| Export presets, `:render` (Phase 8b), frontends | not started |
| Everything else | placeholder crates |

Caveats worth knowing: undo history is not persisted - reopening a project
starts a fresh tree from the saved state - and `ac` resolves to the same range
as `ic` until transitions land in Phase 9f. In `davimci-keys`: `i`/`a`/`r` need
the media picker that comes with the GUI in Phase 9c and report
`NotImplemented`; `gx`/`dax` wait on Phase 9f transitions the same way; `<`/`>`
jump-point edge trims are parsed but not yet wired to a command; visual-mode
track-object narrowing (typing `it`/`at` while a selection is live) is not
implemented - operators in a `VISUAL*` mode act on the whole selection.

In `davimci-analysis`: import and analysis work end to end, but nothing calls
them yet, since there is no frontend to import *into*. Analysis measures the
source, not the post-gain signal, so the cache-invalidation hook for gain and
fade changes has no caller until Phase 9e; predicate searches by clip tag
match nothing until clip tags arrive with the Lua API. Decode, scene
detection, and proxy encoding shell out to `ffmpeg`/`ffprobe`; MLT is used for
preview and export, not for analysis.

In `davimci-cli`: the lifecycle is complete but the binary is a driver, not an
editor - it has no keymap and no display, so `-c` is the only input. `:analyze`
is in spec §12 but belongs to Phase 9e and is not accepted yet, and a recovered
autosave replays into a fresh undo tree, since history is still not persisted.

In `davimci-mlt`: transitions are not projected until Phase 9f, and export
presets arrive in Phase 8b (`RenderSettings` is currently filled in by hand).
`just sanitize` runs clean (ASan/LSan, nightly + `rust-src` via `rustup`) with
a narrow, documented suppression file for MLT's own module-init and
blank-producer state; the refcount guarantees ultimately rest on the wrapper
unit tests, which assert MLT's own `ref_count()` directly.

See `plan.md` for the phase order and `plan.md` milestones for what counts as
usable (M3).

Recent audit fixes, each with a regression test: counts clamp instead of
overflowing on a long digit run (spec §3.1); a backward predicate motion can
no longer answer the frame it started on; new track names take the lowest free
index, so a removed track cannot leave a duplicate name (spec §5); subtitle
cues imported away from frame zero keep their spacing instead of being
dropped; `slip` checks each source handle independently; `move_clip` restores
the clip if the placement is rejected; jump-point stepping is a binary search
rather than a scan per point.

---

## Dev setup

### Arch Linux

```sh
sudo pacman -S --needed mlt ffmpeg clang rust vulkan-swrast
```

| Package | Why |
|---|---|
| `mlt` | Render/preview backend (LGPL-2.1). Headers ship in the main package - no `-dev` needed. Note its pkg-config name is version-suffixed: `mlt-framework-7`, not `mlt-framework`. |
| `ffmpeg` | `ffmpeg` + `ffprobe` for generating test fixtures and verifying exports. |
| `clang` | Required by `bindgen` for the MLT FFI. |
| `rust` | Toolchain, including `clippy` and `rustfmt`. |
| `vulkan-swrast` | Lavapipe. Only needed to run presenter/GUI snapshot tests without a GPU. |

Lua is **not** a system dependency - `mlua` builds a vendored Lua 5.4. Arch's
system Lua is 5.5, which `mlua` does not support, so do not try to link it.

### Debian / Ubuntu

```sh
sudo apt install libmlt-dev libmlt++-dev ffmpeg clang mesa-vulkan-drivers
```

Rust via [rustup](https://rustup.rs) if the distro version is older than the
`rust-version` in `Cargo.toml`.

### Verify

```sh
./scripts/check-env.sh
```

Checks every prerequisite and prints exactly what is missing and how to get it.

---

## Build and run

```sh
cargo build                      # GUI frontend (default)
cargo run -- path/to/video.mkv

cargo build --features tui       # optional TUI + detached preview window
cargo run --features tui -- path/to/video.mkv
```

---

## Testing

```sh
just fixtures        # generate test media with ffmpeg (never committed)
just test            # fast suite - no decode/encode, runs in seconds
just test-slow       # real render/export tests (--features slow-tests)
just test-all        # everything, including sanitizer and GPU snapshot tests
just lint            # clippy (deny warnings) + rustfmt --check
```

Test media is **generated, never committed**. Run `just fixtures` once after
cloning; it writes to `target/fixtures/`.

GPU snapshot tests select lavapipe automatically when no hardware GPU is
present. To force it:

```sh
WGPU_BACKEND=vulkan VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json just test-all
```

---

## Layout

```
crates/
  davimci-core/      timeline model, clips, tracks, grouping, marks, registers
  davimci-cmd/       command objects (apply/invert), undo tree, macro recorder
  davimci-motion/    motions, text objects, jump points, predicate index
  davimci-analysis/  import/conform, waveform, silence, scene change, proxies
  davimci-backend/   RenderBackend trait
  davimci-mlt-sys/   raw FFI bindings
  davimci-mlt/       safe wrapper implementing RenderBackend
  davimci-lua/       Lua API surface, config loader, autocmds
  davimci-keys/      key sequence parser, mode FSM
  davimci-app/       frontend-agnostic view state
  davimci-present/   winit+wgpu video surface
  davimci-gui/       primary frontend
  davimci-tui/       optional terminal frontend
  davimci-headless/  scriptable frontend for tests
  davimci-cli/       binary
```

Two hard rules:

1. Nothing outside `davimci-mlt` may reference MLT types.
2. No frontend may contain view logic - it belongs in `davimci-app` or
   `davimci-present`. The cross-frontend parity test enforces this.

---

## License

GPL-3.0. `libmlt` is LGPL-2.1 and is **dynamically linked**; `melt`/`melted`
(GPL-2) are never linked or vendored. See spec §13.
