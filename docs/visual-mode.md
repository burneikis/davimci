# Visual mode

A selection in davimci is **a time range across a set of tracks**. It is not a
list of clips. Which clips an operator touches is answered against the timeline
at the moment the operator runs, by `Selection::clips`, using overlap.

## The ninety degree turn

Vim selects along one axis: characters run left to right, lines stack downward,
and only `<C-v>` makes a rectangle. A timeline is two real axes - time across,
tracks down - so every visual mode here is a rectangle, and the three modes
differ in **what one cell of that rectangle is** and **how the track set is
built**.

| mode | key | the unit at each end | track set |
|---|---|---|---|
| `VISUAL` | `v` | one frame (see `visualstart`) | contiguous span, `j`/`k` |
| `VISUAL-LINE` | `V` | the whole clip under that end, or the whole gap | contiguous span, `j`/`k` |
| `VISUAL-BLOCK` | `<C-v>` | one frame (see `visualstart`) | contiguous span, plus explicit toggles, so it may have holes |

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

### `:set visualstart=frame|jump`

What `v` and `<C-v>` anchor to on entry, and what each end covers as it moves.

- `frame` (default) - exactly one frame. `v` then `d` deletes one frame.
- `jump` - the jump-point interval containing the frame, `[previous jump point,
  next jump point)`. Useful at a coarse zoom, where a single frame is a
  fraction of a column.

`V` ignores the setting: its unit is always the clip.

## The track axis

`j` and `k` are motions like any other, so in visual mode they move the active
end's **track**, and the selection's track set becomes every track between the
anchor's track and the active end's track, in timeline order. This is the same
rule in all three modes.

`<C-v>` additionally has a toggle key, which adds or removes one track, so a
block selection can skip a track. A later `j` or `k` recomputes the contiguous
span and so discards those holes: toggle after moving, not before.

`it` / `at` in visual replace the track set with the group under the cursor.

## What is drawn

A selection is drawn as the region it is, never as a highlighted clip. A clip
that is half covered is half highlighted: `ClipView::selected_columns` is the
clip's own column range intersected with the selection's, and is `None` for a
clip the selection does not reach. The GUI paints a band over the covered lanes
and tints the covered part of each clip; the terminal inverts the covered cells,
so an empty region of a covered lane still reads as selected.

The mode line carries the extent: `-- VISUAL 12f (V1,A1) --`.
