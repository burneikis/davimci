# Visual mode

A selection in davimci is **a time range across a set of tracks**. It is not a
list of clips. Which clips an operator touches is answered against the timeline
at the moment the operator runs, by `Selection::clips`, using overlap.

## The ninety degree turn

Vim selects along one axis: characters run left to right, lines stack downward,
and only `<C-v>` makes a rectangle. A timeline is two real axes - time across,
tracks down - and there is no reading order that runs off the end of one track
and onto the start of the next. So there is no charwise selection to distinguish
from a blockwise one: **every selection here is a rectangle**, and the two modes
differ only in **what one cell of that rectangle is**.

| mode | key | the unit at each end | track set |
|---|---|---|---|
| `VISUAL` | `v` | one frame (see `visualstart`) | contiguous span, `j`/`k` |
| `VISUAL-LINE` | `V` | the whole clip under that end, or the whole gap | contiguous span, `j`/`k` |

There is no `VISUAL-BLOCK`. `<C-v>` in vim exists to make a rectangle out of a
stream, and here the selection is a rectangle already, so the mode would be an
exact synonym for `v`.

The rule that makes this consistent: a selection has two ends, an **anchor** and
an **active** end. Each end covers a unit, and the selection is the union of the
two units. `v` on one frame therefore selects one frame; `V` on a clip selects
exactly that clip; `V` extended to a second clip selects both clips whole.

## The time axis

Motions (`h`, `l`, `w`, `b`, `f`, `%`, marks, ...) move the **active end**. The
playhead does not move in visual mode - it stays where the selection was
anchored, which is what `o` swaps.

The unit under the active end is recomputed after every motion, so in `V` a
motion that lands inside a clip snaps outward to that clip's bounds.

A motion along time starts from the **edge of what the active end already
covers**, in the direction of travel, not from the point inside it. In `V` on a
whole clip, `h` therefore lands before that clip rather than on its own start
boundary, which the selection already reaches. Every press moves the selection;
a press that selects what was already selected is a bug.

### `:set visualstart=frame|jump`

What `v` anchors to on entry, and what each end covers as it moves.

- `frame` (default) - exactly one frame. `v` then `d` deletes one frame.
- `jump` - the jump-point interval containing the frame, `[previous jump point,
  next jump point)`. Useful at a coarse zoom, where a single frame is a
  fraction of a column.

`V` ignores the setting: its unit is always the clip.

## The track axis

`j` and `k` are motions like any other, so in visual mode they move the active
end's **track**, and the selection's track set becomes every track between the
anchor's track and the active end's track, in timeline order. This is the same
rule in both modes.

## Leaving a track out

There is no way to select V1 and A2 while skipping the A1 between them: the
track set is the contiguous span between the two ends, in both modes.

`it` / `at` in visual replace the track set with the group under the cursor.

## What is drawn

A selection is drawn as the region it is, never as a highlighted clip. A clip
that is half covered is half highlighted: `ClipView::selected_columns` is the
clip's own column range intersected with the selection's, and is `None` for a
clip the selection does not reach. The GUI paints a band over the covered lanes
and tints the covered part of each clip; the terminal inverts the covered cells,
so an empty region of a covered lane still reads as selected.

The mode line carries the extent: `-- VISUAL 12f (V1,A1) --`.

## Where the cursor is

The playhead is one time for the whole timeline, so it is drawn down every
lane - which on its own never says which track an edit would land on. The
*cursor* is that answer: the playhead column on the focused lane, drawn bright
where the rest of the column is dim, with the focused track's header lit and its
name marked (`>V1` in the terminal). Everything anchored "under the cursor",
including entry into either visual mode, means that lane at that frame.
