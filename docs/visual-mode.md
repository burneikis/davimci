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

A motion along time starts from the **edge of what the active end already
covers**, in the direction of travel, not from the point inside it. In `V` on a
whole clip, `h` therefore lands before that clip rather than on its own start
boundary, which the selection already reaches. Every press moves the selection;
a press that selects what was already selected is a bug.

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

So `v` and `<C-v>` cover the same frames given the same motions; the only
difference is that `<C-v>` can hold a track set with holes in it, which is the
one thing `v` can never express. Reach for `<C-v>` when you want V1 and A2 but
not A1.

### `<C-v>` in the window

The window never receives `Ctrl+V` as a key: the winit layer takes it as the
platform paste chord and emits a paste event instead. `translate_events` reads a
paste back as `<C-v>` whenever no modal is spelling out a line, and as the
pasted text when one is. The single case this cannot recover is `Ctrl+V` with an
empty clipboard, where no event is emitted at all.

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
including entry into every visual mode, means that lane at that frame.
