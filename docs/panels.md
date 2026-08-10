# Plugin panels

A panel is a floating box a plugin owns and every frontend draws. It is view
state, never project state: opening one is not an edit, reaches no `Command`,
and leaves nothing in the undo log.

The app places every panel and hands the placement to the frontends, so the
GUI and the TUI cannot disagree about where a panel is. A frontend only
blits.

## Opening one

```lua
local ui = require("davimci.ui")

local p = ui.panel({
  title  = "notes",       -- optional
  anchor = "bottom-left", -- center, top-left, top-right,
                          -- bottom-left, bottom-right, playhead
  columns = 30,           -- optional; content decides the size otherwise
  rows    = 6,
  z       = 10,           -- draw order; ties break by open order
})

p:set_lines({
  "a plain line",
  { { text = "d", role = "key" }, "  delete" },
})

p:show()
p:hide()
p:close()
```

A line is either a string or a list of spans. A span is a string or
`{ text = ..., role = ... }`, where the role is one of `normal`, `key`,
`accent` or `warning`. Roles pick colours; the text is identical in every
frontend.

## Pictures

```lua
p:set_picture({ width = 64, height = 64, rgba = pixels })
```

`rgba` is a string of `width * height * 4` bytes. The window uploads it as a
texture; a terminal has no pixels, so it draws the title and a placeholder
instead. That is a local degradation, not an error.

## Sizes

Panels are measured in **character cells**: glyphs across, text lines down -
the one unit a terminal and a window both have. They are drawn over the whole
editing area (ruler, video pane and lanes), *not* over the lanes alone, so a
panel's height is bounded by the screen rather than by how many tracks the
project happens to have. Only the status and `:` lines stay clear.

Sizes are requests: a panel is always clamped to that area, so an oversized
panel is drawn big rather than drawn off screen.

Panels are capped per host (16 open, 200 lines, 64 spans a line) and control
characters are stripped from their text. A plugin cannot push the editor off
screen or move a terminal's cursor.

## Focus

```lua
ui.panel({
  focus = true,
  on_key = function(key) ... end,   -- "j", "<Esc>", "<Enter>", ...
})
```

A focused panel owns the keyboard while it is open. It is asked *after* the
`:` line, the media picker and the subtitle editor, so a plugin can never
take an editor modal's keys. `focus` without `on_key` is refused at open
time.

`<Esc>` always closes a focused panel *and* is still delivered, so a plugin
that stops answering cannot hold the keyboard.

Panels are unfocused by default, which is what lets a reporting panel like
which-key exist without ever eating a keystroke.

## `KeyPending`

```lua
require("davimci.autocmd").on("KeyPending", function(e)
  e.mode           -- "NORMAL", ...
  e.keys           -- what has been typed, e.g. "3g"; "" when idle
  e.continuations  -- { { key = "g", description = "...", group = false } }
end)
```

The keymap is the only thing that knows which keys are live, so a plugin
reads it from here rather than keeping a copy. The bundled which-key plugin
(`crates/davimci-cli/runtime/plugins/which-key/plugin/init.lua`) is a view of exactly
this event and nothing else. It is opt-in; see `docs/plugins.md` for how a
config turns a bundled plugin on.
