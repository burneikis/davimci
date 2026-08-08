# Default keymap

Generated from the keymap table in `davimci-keys`; do not edit by hand.
Run `just docs` after changing a binding.

Counts, registers, marks and text objects are grammar, not bindings:
`3dw`, `"ay`, `` `a `` and `dic` compose out of the entries below.

What `v` and `V` select, and how `j`/`k` widen a selection across
tracks, is in `docs/visual-mode.md`.

## Audio

| Keys | Action |
|---|---|
| `+` | adjust gain by +1 dB |
| `-` | adjust gain by -1 dB |
| `<Space>m` | mute or unmute the focused track |
| `<Space>s` | solo or unsolo the focused track |

## Editing

| Keys | Action |
|---|---|
| `.` | repeat the last edit |
| `>` | trim the nearest edge one jump point later |
| `a` | append media after the current clip |
| `dax` | delete the transition at the nearest cut |
| `gp` | paste after the playhead, overwriting |
| `gP` | paste before the playhead, overwriting |
| `gs` | split at the playhead on every track |
| `gx` | create a transition at the nearest cut |
| `i` | insert media at the playhead |
| `p` | paste after the playhead, rippling |
| `P` | paste before the playhead, rippling |
| `r` | replace the clip under the playhead |
| `s` | split at the playhead on the focused track |
| `u` | undo |
| `x` | ripple delete the clip under the playhead |
| `<` | trim the nearest edge one jump point earlier |
| `<C-r>` | redo |

## Marks and macros

| Keys | Action |
|---|---|
| `@` | replay the macro in the register named by the next key |
| ``` | jump to a mark, named by the next key |
| `m` | set a mark, named by the next key |
| `q` | record a macro into the register named by the next key |

## Motions

| Keys | Action |
|---|---|
| `$` | end of the timeline |
| `%` | the other end of the current clip |
| `0` | start of the timeline |
| `[t` | cycle track focus back, wrapping |
| `]t` | cycle track focus forward, wrapping |
| `b` | previous clip boundary |
| `e` | last frame of the current clip |
| `G` | end of the timeline |
| `gg` | start of the timeline |
| `h` | previous jump point |
| `j` | focus the next track |
| `k` | focus the previous track |
| `l` | next jump point |
| `w` | next clip boundary |
| `{` | previous marker |
| `}` | next marker |
| `<Left>` | one frame back |
| `<Right>` | one frame forward |

## Operators

| Keys | Action |
|---|---|
| `c` | change: delete, then insert (takes a motion or object) |
| `d` | ripple delete (takes a motion or object) |
| `f` | fade across the range (takes a motion or object) |
| `gd` | lift: delete and leave a gap (takes a motion or object) |
| `gt` | roll the nearest cut (takes a motion or object) |
| `gT` | slide the clip under the playhead (takes a motion or object) |
| `t` | ripple trim the nearest edge (takes a motion or object) |
| `T` | slip the clip under the playhead (takes a motion or object) |
| `y` | yank (takes a motion or object) |

## Transport

| Keys | Action |
|---|---|
| `H` | shuttle back |
| `L` | shuttle forward |
| `<Space><Space>` | play or pause |
| `<Space>l` | loop the selection |
| `<Space>p` | preview, then return to the playhead |

## View and modes

| Keys | Action |
|---|---|
| `:` | open the `:` line |
| `z0` | reset the zoom level |
| `zi` | zoom in one level |
| `zo` | zoom out one level |
| `zz` | centre the view on the playhead |
| `zZ` | keep the playhead centred |
| `<Esc>` | leave the current mode |

## Visual mode

| Keys | Action |
|---|---|
| `o` | swap the ends of the selection |
| `v` | select from the frame under the cursor (visual) |
| `V` | select whole clips (visual-line) |
