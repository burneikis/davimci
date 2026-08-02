# vimci

A keyboard-first, modal video editor. Vim motions, verbs, and modes for
cutting footage, trimming audio, compositing overlays, and adding subtitles.
Configured like Neovim: `~/.config/vimci/init.lua`, a Lua scripting API,
remappable keys, and hookable events.

- [`spec.md`](spec.md) - what it is and how it behaves
- [`plan.md`](plan.md) - how it gets built, and how it gets tested

## Status

<!-- Keep this current. It must never claim more than the code does. -->

**Phase 5 complete - media import, conform, and analysis.** Phase 6 (the MLT
render backend) is next. Workspace builds; `just test` and `just lint` are
green, and `just fixtures && just test-slow` passes against generated media.

Nothing is runnable yet: `vimci-cli` is still a placeholder, and the model has
no backend and no frontend (see plan.md milestone M1). `vimci-keys` can drive
a `Session` end-to-end from raw key strings in tests, but nothing feeds it
real keyboard events yet.

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
| Analysis cache: `.vimci/cache/<hash>.analysis`, versioned, corruption-tolerant | implemented, tested |
| Background jobs: progress, cancellation, cancel-on-close | implemented, tested |
| Proxies: threshold rule, frame-exact spec, `BeforeExport` original-source guard | implemented, tested |
| Backend, Lua, frontends | not started |
| Everything else | placeholder crates |

Caveats worth knowing: undo history is not persisted - reopening a project
starts a fresh tree from the saved state - and `ac` resolves to the same range
as `ic` until transitions land in Phase 9f. In `vimci-keys`: `i`/`a`/`r` need
the media picker that comes with the GUI in Phase 9c and report
`NotImplemented`; `gx`/`dax` wait on Phase 9f transitions the same way; `<`/`>`
jump-point edge trims are parsed but not yet wired to a command; visual-mode
track-object narrowing (typing `it`/`at` while a selection is live) is not
implemented - operators in a `VISUAL*` mode act on the whole selection.

In `vimci-analysis`: import and analysis work end to end, but nothing calls
them yet, since there is no frontend to import *into*. Analysis measures the
source, not the post-gain signal, so the cache-invalidation hook for gain and
fade changes has no caller until Phase 9e; predicate searches by clip tag
match nothing until clip tags arrive with the Lua API. Decode, scene
detection, and proxy encoding shell out to `ffmpeg`/`ffprobe` - MLT does not
enter the picture until Phase 6.

See `plan.md` for the phase order and `plan.md` milestones for what counts as
usable (M3).

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
  vimci-core/      timeline model, clips, tracks, grouping, marks, registers
  vimci-cmd/       command objects (apply/invert), undo tree, macro recorder
  vimci-motion/    motions, text objects, jump points, predicate index
  vimci-analysis/  import/conform, waveform, silence, scene change, proxies
  vimci-backend/   RenderBackend trait
  vimci-mlt-sys/   raw FFI bindings
  vimci-mlt/       safe wrapper implementing RenderBackend
  vimci-lua/       Lua API surface, config loader, autocmds
  vimci-keys/      key sequence parser, mode FSM
  vimci-app/       frontend-agnostic view state
  vimci-present/   winit+wgpu video surface
  vimci-gui/       primary frontend
  vimci-tui/       optional terminal frontend
  vimci-headless/  scriptable frontend for tests
  vimci-cli/       binary
```

Two hard rules:

1. Nothing outside `vimci-mlt` may reference MLT types.
2. No frontend may contain view logic - it belongs in `vimci-app` or
   `vimci-present`. The cross-frontend parity test enforces this.

---

## License

GPL-3.0. `libmlt` is LGPL-2.1 and is **dynamically linked**; `melt`/`melted`
(GPL-2) are never linked or vendored. See spec §13.
