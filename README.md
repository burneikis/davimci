# davimci

A keyboard-first, modal video editor. Vim motions, verbs, and modes for
cutting footage, trimming audio, compositing overlays, and adding subtitles.
Configured like Neovim: `~/.config/davimci/init.lua`, a Lua scripting API,
remappable keys, and hookable events.

- [`todo.md`](todo.md) - loose ends and deferred work
- [`docs/keymap.md`](docs/keymap.md) - the default keymap, generated from the code
- [`docs/plugins.md`](docs/plugins.md) - what is core, what is a plugin, and how to
  turn a bundled plugin on

## Usage

```sh
davimci clip.mkv                        # open the editor window
davimci clip.mkv -k "ll<Right>s"        # same editor, scripted, no window
davimci clip.mkv -k "  " --ticks 30     # play, pulling real frames through MLT
davimci project.davimci -c ':w'         # project lifecycle from the command line
davimci clip.mkv -c ':export out.mkv' --no-window   # batch export, with progress
davimci --script session.dvs            # keystrokes plus assertions, from a file
```

`-k` drives the whole stack - key grammar, commands, MLT backend, presenter,
transport - with a headless frontend standing in for the window, which is how
the editor is tested without a display. `--no-window` keeps any invocation on
the command line.

## Dev setup

### Arch Linux

```sh
sudo pacman -S --needed mlt ffmpeg clang rust vulkan-swrast
```

| Package | Why |
|---|---|
| `mlt` | Render/preview backend (LGPL-2.1). Headers ship in the main package. Its pkg-config name is version-suffixed: `mlt-framework-7`. |
| `ffmpeg` | `ffmpeg` + `ffprobe` for generating test fixtures and verifying exports. |
| `clang` | Required by `bindgen` for the MLT FFI. |
| `rust` | Toolchain, including `clippy` and `rustfmt`. |
| `vulkan-swrast` | Lavapipe. Only needed to run presenter/GUI snapshot tests without a GPU. |

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

## Build and run

```sh
cargo build                      # GUI frontend (default)
cargo run -- path/to/video.mkv

cargo build -p davimci-cli --features tui    # optional terminal frontend
cargo run -p davimci-cli --features tui -- --tui path/to/video.mkv
```

<!-- THIS NEEDS REVISING -->
<!-- Seeing the timeline is core, so every build ships a frontend that can show -->
<!-- one; which one is the build's choice. A build with neither `window` nor `tui` -->
<!-- is refused at compile time unless it asks for `--features driver-only`, the -->
<!-- scripted driver the tests and batch exports run through. `just weigh` prints -->
<!-- what each profile links, against its budget. -->

## Hardware acceleration

<!-- CLEAN THIS UP TOO -->
<!-- Optional, off by default, and never required: davimci runs fully on the CPU, -->
<!-- which is the path every test asserts against. Three runtime switches turn the -->
<!-- fast paths on, and each falls back to software with a sentence saying why -->
<!-- rather than failing. -->
<!---->
<!-- ``` -->
<!-- :set decode cpu|auto   # VAAPI decode for long-GOP sources that benefit -->
<!-- :set encode cpu|auto   # a hardware encoder where it meets the export preset -->
<!-- :set proxy on|off      # proxy media for qualifying imports -->
<!-- ``` -->
<!---->
<!-- A window with a `wgpu` device uploads the decoder's YUV planes and converts -->
<!-- them in a shader, which is three eighths of the bytes of an RGBA upload and -->
<!-- no CPU colour conversion; a machine without one composites on the CPU and -->
<!-- looks identical. An export preset may demand hardware with `hardware = true`, -->
<!-- in which case an export that cannot deliver it is refused rather than -->
<!-- silently encoded in software. -->
<!---->
<!-- None of this is what makes a build heavy: the shader adds no dependency the -->
<!-- window did not already pull in, and the decode and encode switches are -->
<!-- runtime choices inside MLT. See `docs/plugins.md` for the weight budgets. -->

## Testing

```sh
just fixtures        # generate test media with ffmpeg (never committed)
just test            # fast suite - no decode/encode, runs in seconds
just test-slow       # real render/export tests (--features slow-tests)
just test-gpu        # the planar shader path, against the CPU conversion
just test-all        # everything, including sanitizer and GPU snapshot tests
just perf            # timing budgets and scaling checks, in release
just bench           # criterion benchmarks
just soak-asan       # the soak fuzz under AddressSanitizer
just weigh           # what each build profile links, against its budget
just docs            # regenerate docs/keymap.md
just lint            # clippy (deny warnings) + rustfmt --check
```

A scripted session is a `.dvs` file of `keys`/`cmd`/`expect` lines; every file
in `crates/davimci-headless/tests/sessions/` is a test, and the same file
replays through the real editor with `just script <file>`.

Test media is generated, never committed. Run `just fixtures` once after
cloning; it writes to `target/fixtures/`.

GPU snapshot tests select lavapipe automatically when no hardware GPU is
present. To force it:

```sh
WGPU_BACKEND=vulkan VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json just test-all
```

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

## License

GPL-3.0. `libmlt` is LGPL-2.1 and is dynamically linked; `melt`/`melted`
(GPL-2) are never linked or vendored.
