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
  preview, seek, scrub and encode, including the measurements analysis takes
  (loudness hops, detected scene changes). Measuring is core; what the
  numbers mean for an edit is not.
- One transition type, `dissolve`. It is the fallback an unregistered name
  renders as, so a project always opens.
- A clip's text payload and the commands that write it: `SetClipText`, and
  `:track` / `:text`, which make a track and put a cue on it. Creating
  what the model can hold is core however few people want it. What `i` means
  on a text track, and cue-to-cue movement, are not - those are the
  `text` plugin.
- The view: timeline lanes, ruler, video pane, status line and `:` line
  (`davimci-app`, `davimci-present`, `davimci-gui`, `davimci-tui`).
- The plugin surface itself: the Lua runtime, the event list, panels, and the
  request queue that turns a plugin's intent into a command.

## What is a plugin

A thing is a plugin when it is a *view* of state core already has, or an
opinion about how to edit that the model does not need to hold.

- Anything that only reads events and draws: which-key above all.
- The transition catalogue: every wipe and iris is a `luma` plus a geometry,
  which is a registration, not a backend feature. Export presets and effect
  chains likewise.
- Analysis-driven editing: where a scene cut is worth landing on, what
  loudness counts as silence, beat detection as a jump source.
- Audio processing beyond the built-in duck: EQ, compression, noise
  reduction.
- Workflow opinions: proxy policies, naming schemes, per-project layouts.

If a bundled plugin needs something `davimci.*` cannot express, that is a gap
in the API, not a reason to special-case it in the host. Bundled plugins are
written against exactly the surface a third-party plugin gets.

## Bundled plugins

Bundled plugins are ordinary plugins that happen to ship in the binary:
each is a directory under `crates/davimci-cli/runtime/plugins/<name>/` with
the same `davimci.toml` and `plugin/init.lua` an installed plugin has, and
nothing about one is written in Rust. They are examples as much as features.
They run before the rest of the user config, so a config can rebind or
replace anything they set up.

| Plugin | Default | Owns | What it does |
|---|---|---|---|
| `transitions` | off | `wipe_left`, `wipe_right`, `wipe_up`, `wipe_down`, `iris` | The video transition catalogue: a `luma` plus a geometry. |
| `silence` | off | `next_silence`, `prev_silence` | Those motions and `]s` / `[s`, over a threshold you can change. |
| `scenes` | off | `next_scene`, `prev_scene` | Those motions and `]v` / `[v`, over the detected cuts. |
| `text` | off | `next_text`, `prev_text`, `text` tracks | `]c` / `[c` cue to cue, and `i` on a text track editing the cue under the playhead instead of inserting media. Subtitle tracks are text tracks, so imported cues land here too. Turns itself on when a text track appears, whether from an import or from `:track text`. |
| `which-key` | off | - | Lists what can follow a half-typed key sequence. |

**Every bundled plugin is off.** A default that is on in practice is core
wearing a plugin's name, so nothing here runs until something asks for it.
With all of them off there is still a timeline, a grammar, a preview and an
export - which is the test of whether a thing belongs in this table at all.

## Nothing is lost by defaulting to off

Off would be a trap if a project written elsewhere quietly lost a transition,
so a plugin declares the names it owns (`Bundled::provides`) and the session
turns it on when one of those names comes up.

- **Opening a project** that uses `wipe_left` switches `transitions` on and
  says so in the status line. The file is what asks; the user did not have to
  know a plugin existed.
- **Opening a project with a text track** switches `text` on, so cues
  written elsewhere stay editable. The clip text and `SetClipText` are core -
  they are the model and its write path - but the workflow over them is not.
- **Calling a motion** nothing registered names its owner: "the motion
  `next_silence` comes from the bundled `silence` plugin; enable it in
  `plugins.lua`". A missing name is never a silent no-op.
- **An explicit `disable`** outranks both. The project still opens, the wipe
  still renders as a `dissolve`, and the status line says that is what
  happened rather than leaving it to be noticed.

So the split is honest in both directions: a fresh install draws and binds
only what core does, and no file, macro or config silently loses meaning
because a plugin was off.

## Choosing plugins

`~/.config/davimci/plugins.lua` says which bundled plugins to run. It is read
in its own pass before any of them execute, and it is not run again as part
of the ordinary config load.

```lua
local plugins = require("davimci.plugins")

plugins.enable("which-key")
plugins.disable("which-key")
plugins.enable({ "silence", "scenes" })
plugins.setup({ ["which-key"] = true, transitions = false })
```

Naming no plugin is an error: an empty `enable()` is a typo, not a no-op.

Saying nothing about a plugin leaves it off until a project or a call needs a
name it owns. Saying `disable` means off even then - that is the one way to
keep a plugin from ever running, and the editor reports what the project
loses rather than pretending nothing did.

## Installing plugins

davimci ships the loading mechanism, not a package manager. Fetching a plugin
from anywhere is a separate program's job; what the editor guarantees is
where a plugin goes, when it runs, and what it may assume about the host.

Packages live under `$XDG_DATA_HOME/davimci/site`:

```text
site/pack/<group>/start/<plugin>/   runs at startup
site/pack/<group>/opt/<plugin>/     runs when davimci.pack.add names it
```

The group level exists so a fetcher can own one directory without touching
what a user dropped in by hand. A plugin directory is laid out like the
config directory, so a config tree can be moved into a package unchanged:

```text
beats/
  davimci.toml        what the host may know without running anything
  plugin/init.lua     run on load
  lua/beats/grid.lua  require("beats.grid")
  motions/*.lua
  presets/*.lua
```

`require` searches the `lua/` directory of every package, then the config
root last, so a package can never shadow a module the user wrote.

## Manifests

```toml
name = "beats"          # must match the directory it is installed as
version = "0.3.1"
api = "^1.0"            # the davimci.* surface it was written against
requires = ["aubio"]    # external programs, reported rather than checked

[provides]
motions = ["next_beat", "prev_beat"]
transitions = []
track_kinds = []
```

A manifest is declarative and is never executed, so the host can answer "who
owns `wipe_left`?" without running a stranger's Lua. `api` is a range over
`davimci.api_version`, which moves independently of the binary's version: a
plugin outside the range is refused with a sentence rather than run, because
a plugin written for another API asks for edits this host would misread. A
directory with no manifest still loads - the manifest is what buys the host
the ability to speak for the plugin before it runs.

## Load order

1. `plugins.lua`, alone, so every choice is known before anything runs.
2. Bundled plugins that are enabled.
3. `start` packages, in group then name order.
4. `opt` packages named by `davimci.pack.add`, in the order named.
5. `init.lua`, `keymaps.lua`, then `motions/`, `presets/`, `plugin/` in the
   config root.
6. The project-local `.davimci.lua`, if it is trusted.

Plugins run before the user's own files, so a config always wins over a
plugin without either knowing about the other. Failures stay isolated per
file: one broken plugin costs you that plugin, not the editor.

```lua
-- ~/.config/davimci/plugins.lua
require("davimci.plugins").enable({ "silence", "which-key" })
require("davimci.pack").add("proxies")   -- an opt package, wanted today
```
