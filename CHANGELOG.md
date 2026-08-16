# Changelog

Guidelines:

- One entry per user-visible change. Internal refactors, test-only work, and CI
  changes are not entries.
- Write for someone using the editor, not for someone reading the diff: name the
  key, command, or behaviour that changed.
- Add entries under `Unreleased` as part of the PR that makes the change.
- Group under `Added`, `Changed`, `Fixed`, or `Removed`, in that order. Drop
  empty groups.
- Breaking changes to keymaps, the Lua API, or the project format start with
  `BREAKING:` and say what to do instead.
- On release, rename `Unreleased` to the version and date, and open a fresh
  empty `Unreleased`.

## Unreleased

## 0.1.1 - 2026-08-16

### Changed

- Preview decodes on more than one thread on a multi-core machine, instead of
  the single thread MLT defaults to. `DAVIMCI_DECODE_THREADS` and
  `DAVIMCI_REAL_TIME` override the choice.
- Automatic resolution reduction now answers only the stutter a smaller decode
  can cure. Frames late because the frontend drew too slowly no longer soften
  the picture for no gain.

### Fixed

- Automatic resolution changes during playback no longer stop and re-seek the
  preview, so the picture and sound run through the change instead of freezing
  for about a second.
- Pausing repaints at the restored resolution, so a paused frame is no longer
  left soft until the playhead moves.
- The TUI loop keeps its tick period instead of adding drawing time on top of
  it, so a 60fps timeline is no longer played slower than the source with most
  frames thrown away.
- Preview plays at the speed it was asked for on a machine with no audio
  output. Without a sound card the audio consumer had nothing to wait for and
  raced through the timeline, so a backwards shuttle showed one picture and
  froze; wall time now keeps the clock when audio cannot.
- Preview no longer fails outright when the preferred audio output exists but
  cannot be started; the next one is tried instead.

## 0.1.0 - 2026-08-13

First public release.

### Added

- Modal editing over a timeline: normal, insert, visual, and command-line
  modes, with counts, operators, and `.`-repeat behaving as they do in vim.
- Motions for the playhead and the clip under it - frames, seconds, clip
  edges, marks, and jump points - usable on their own or as the target of an
  operator. `docs/keymap.md` lists the full default map.
- Operators for cutting, trimming, deleting, yanking, and pasting clips, with
  ripple and non-ripple forms.
- Visual mode over frame ranges and clip selections, including clip grouping
  with per-group colouring. See `docs/visual-mode.md`.
- Marks, registers, and recordable macros.
- Unlimited undo and redo. Every mutation is a command that returns its own
  inverse, so undo covers the Lua API, macros, and `.`-repeat alike.
- Multi-track video and audio timelines, with clips movable between tracks.
- Playback with real frames pulled through MLT, variable-speed shuttle in both
  directions, backward audio, and an optional centred playhead.
- Three frontends over one shared view model: a GUI window, a TUI, and a
  headless frontend. `-k` scripts keystrokes through the whole stack and
  `--no-window` keeps any invocation on the command line.
- Panels for the preview, timeline, and inspectors, with configurable sizes and
  focus. See `docs/panels.md`.
- Project lifecycle from the command line: open, `:w`, and `:export` with
  progress, plus autosave.
- Lua configuration from `~/.config/davimci/init.lua` - remappable keys,
  hookable events, and a scripting API onto the same command layer the keymap
  uses.
- Bundled plugins, all off by default: `audio`, `presets`, `proxies`,
  `scenes`, `silence`, `text`, `transitions`, and `which-key`. See
  `docs/plugins.md`.
- Plugin manager and fetcher, plugin manifests, deterministic load order, and
  `:checkhealth`.
- Proxy media with hardware-accelerated decode and encode where available.
- Transitions with live preview, and a text/overlay compositor.
- `.dvs` session scripts: keystrokes plus assertions, run with `--script`.
- `scripts/install.sh` for a checksum-verified release build, falling back to
  building from source when the platform has no release asset. `just install`
  does the source path from a clone.
