# vimci

A keyboard-first, modal video editor. Vim motions, verbs, and modes for
cutting footage, trimming audio, compositing overlays, and adding subtitles.
Configured like Neovim: `~/.config/vimci/init.lua`, a Lua scripting API,
remappable keys, and hookable events.

- [`spec.md`](spec.md) - what it is and how it behaves
- [`plan.md`](plan.md) - how it gets built, and how it gets tested

Status: **early scaffolding.** Spec and plan are complete. The workspace
builds, the error model and frame-time core are implemented and tested;
everything else is a placeholder crate. See `plan.md` for phase order.

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
