# Core and plugins

The editor is the timeline model, the write path into it, and the two ways
of looking at that: the grammar you type and the pictures you see. Everything
else is a plugin, bundled or not.

## What is core

A thing is core when removing it would leave no editor at all, or when it is
the only write path to something the model owns.

- The timeline model: tracks, clips, `Frame(u64)` time, one framerate and one
  resolution (`davimci-core`).
- Commands, undo, `.`-repeat and macros (`davimci-cmd`). Nothing may write to
  a timeline except through a command, so this can never be optional.
- Motions, text objects and the key grammar (`davimci-motion`,
  `davimci-keys`): the grammar is the interface, not a feature of it.
- Project load, save and autosave, including conforming a source on import.
- The render backend seam and export (`davimci-backend`, `davimci-mlt`):
  preview, seek, scrub and encode.
- The view: timeline lanes, ruler, video pane, status line and `:` line
  (`davimci-app`, `davimci-present`, `davimci-gui`, `davimci-tui`).
- The plugin surface itself: the Lua runtime, the event list, panels, and the
  request queue that turns a plugin's intent into a command.

## What is a plugin

A thing is a plugin when it is a *view* of state core already has, or an
opinion about how to edit that the model does not need to hold.

- Anything that only reads events and draws: which-key above all.
- Extra transition types, export presets, and effect chains beyond what the
  backend already exposes by name.
- Analysis-driven editing: scene detection, beat detection, silence-driven
  cutting policy.
- Audio processing beyond the built-in duck: EQ, compression, noise
  reduction.
- Workflow opinions: proxy policies, naming schemes, per-project layouts.

If a bundled plugin needs something `davimci.*` cannot express, that is a gap
in the API, not a reason to special-case it in the host. Bundled plugins are
written against exactly the surface a third-party plugin gets.

## Bundled plugins

Bundled plugins are compiled into the binary and listed in
`davimci_cli::BUNDLED` (sources in `crates/davimci-cli/runtime/plugins/`).
They run before the rest of the user config, so a config can rebind or
replace anything they set up.

| Plugin | Default | What it does |
|---|---|---|
| `which-key` | off | Lists what can follow a half-typed key sequence. |

A plugin is on by default only when the editor would feel broken without it.
Anything that changes what is on screen is opt-in, so a fresh install draws
what the core view draws and nothing more.

## Choosing plugins

`~/.config/davimci/plugins.lua` says which bundled plugins to run. It is read
in its own pass before any of them execute, and it is not run again as part
of the ordinary config load.

```lua
local plugins = require("davimci.plugins")

plugins.enable("which-key")
plugins.disable("which-key")
plugins.enable({ "which-key" })
plugins.setup({ ["which-key"] = true })
```

Naming no plugin is an error: an empty `enable()` is a typo, not a no-op.
Saying nothing about a plugin leaves it at whatever it ships as, which is why
a config never has to list the ones it does not care about.
