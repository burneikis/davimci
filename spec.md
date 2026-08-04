# Vim-Motion Video Editor - Spec

## 1. Overview

A keyboard-first, modal video editor for cutting down footage, trimming audio, compositing overlays, and adding text/subtitles - controlled with vim-style motions, verbs, and modes. Configured like Neovim: a `.config/davimci/init.lua` entrypoint with a Lua scripting API, remappable keys, and hookable events.

Primary workflow this is designed around:
1. Import source video (MKV + others)
2. Cut down / ripple-delete unwanted sections
3. Remove talking/noise from specific audio tracks
4. Layer images/video on top (overlays, B-roll)
5. Add subtitles / text layers

---

## 2. Core Concepts

### 2.1 The "Buffer" = Timeline

- The timeline is the buffer. It contains **tracks** (video, audio, text/subtitle, overlay), stacked top to bottom.
- The **playhead** is the cursor. It sits at a frame position on the time axis, and (for track-scoped ops) also has a "current track" or "current track group."
- A **clip** is roughly vim's "word" - the atomic movable/splittable/deletable unit.
- A **segment** is a sub-range within a clip (used for partial visual selection, e.g. selecting 2s out of a 10s clip without splitting it first).

### 2.2 Modes

| Mode | Purpose |
|---|---|
| `NORMAL` | Navigate playhead/tracks, issue verb+motion commands |
| `VISUAL` | Select a time range and/or track scope, then act on it |
| `VISUAL-BLOCK` (track-block) | Select same time range across multiple specific tracks (e.g. Video1 + Audio2 but not Audio1) |
| `INSERT` | Actively placing/importing media at the playhead |
| `COMMAND` (`:`) | Ex-style commands: export, save project, jump to timecode, run scripts |

Mode is shown in a status line near the playhead (e.g. `-- VISUAL (V1,A2) --`).

---

## 3. Movement (Nouns / Motions)

### 3.1 Frame-accurate movement

- Arrow keys (`←`/`→`): **always** frame-by-frame, regardless of zoom. Not remapped by default - a fixed "precision" fallback. Configurable to disable.
- `h` / `l`: move playhead by **N relative jump points**, where N depends on zoom level (see 3.2).
- `5h` / `5l`: jump 5 points left/right (count-prefixed, standard vim numeric-prefix behavior).
- Counts are clamped to 1,000,000, and an operator count multiplied by a
  motion count clamps to the same ceiling. A long digit run is pinned like
  vim's, never rejected and never wrapped.
- `j` / `k`: move current-track focus down/up (between tracks in the track stack).

### 3.2 Relative jump points (zoom-aware scrubbing)

- At any zoom level, the timeline computes a set of **jump points**: clip
  boundaries, markers, cut points, and (if zoomed in enough) evenly spaced
  sub-divisions between them.
- `h`/`l` moves to the *next visible jump point* in that direction - this is
  the "ez scrubbing" behavior. Zoomed out → jump points are far apart (clip-level).
  Zoomed in → jump points are dense (near frame-level).
- Jump points are rendered as small tick marks on the timeline ruler so the
  user can see where `h`/`l` will land before pressing it.
- Every tick carries a **relative number**, subdivisions included: the count
  of jump points between it and the playhead, so `3l` visibly lands on the
  tick labelled `3`, exactly as vim's `relativenumber` shows how far `3j`
  goes. The point at or before the playhead is `0`, and direction is read
  from the side of the playhead a tick is on rather than from a sign. Where
  two numbers would overlap the later one is dropped, so a dense ruler thins
  out instead of smearing.
- Configurable: `jump_point_density`, and whether jump points snap to
  (clip bounds | markers | beat-detected audio peaks | silence boundaries).
- Density is **monotonic in zoom**: zooming in only ever adds points, never
  moves or removes one, so a landing spot never shifts under the user. Below a
  configurable zoom level there are no subdivisions at all and `h`/`l` are
  purely clip- and marker-level; above it, subdivision spacing halves per level
  down to one frame.
- Subdivision spacing is defined in **screen columns**, not frames: a
  subdivision every `columns_per_subdivision` columns (default 8). Since frames
  per column also halves per zoom level, the spacing halves per level as above
  while the *on-screen* tick density stays constant, so zooming in twice never
  turns a screen into hundreds of jump points.
- Frame zero and the end of the timeline are always jump points.

### 3.2.1 Transport / playback

Playback is a first-class mode-independent action, not a motion. `<Space>` is
the **leader** key; pressing it twice is play/pause, so the most common action
is also the easiest to reach without spending a dedicated key.

| Key | Action |
|---|---|
| `<Space><Space>` | Play / pause toggle |
| `L` / `H` | Shuttle forward / backward; press repeatedly to increase speed (1x, 2x, 4x, 8x); pressing the opposite key decelerates through 1x before reversing |
| `<Space>p` | Play from playhead, return playhead to origin on stop (preview-and-return) |
| `<Space>l` | Loop the current selection (or current clip in NORMAL) |

`<Space>l` loops what is selected, or the clip under the playhead in NORMAL.
The loop is transport state, never an edit: it never reaches the undo log, it
survives a pause and a seek *inside* its range, and pressing `<Space>l` again
on the same range turns it off. Playback wraps to the loop's start rather than
stopping at its end, and starts from the loop's start when the playhead is
outside it. The loop ends, with a message, when the selection it was set on is
cleared or the playhead seeks out of its range.

Shuttle is varispeed playback where the backend can vary its rate: the audio
clock keeps running and the rate steps, so a shuttle sounds like a shuttle. A
backend without rate control degrades to a silent stepped scrub at the same
speeds and on the same keys, rather than refusing.

**Backwards shuttle is always a stepped scrub**, silent, even on a backend with
rate control: audio consumers do not run backwards, and a negative producer
speed stalls the clock instead of playing in reverse. Reversing therefore stops
the audio and walks the playhead back at the same speeds; accelerating forward
again resumes varispeed playback. A backwards shuttle that reaches frame 0
stops there, and never commits the playhead to the end of the timeline.

Shuttle is available whenever the transport is idle: `H` or `L` from a paused
NORMAL mode starts one, rather than requiring playback first.

Shuttle is `H`/`L` rather than the JKL of other NLEs: the fingers are already
on `h`/`l` for frame motion, so the shifted pair is the same gesture at speed.
Lowercase `h`/`j`/`k`/`l` keep their vim meanings (frame/jump motion, track
focus).

There is **no default stop binding**. A shuttle is left either by decelerating
through zero with the opposite key or by `<Space><Space>`, which stops motion
of any kind. The `shuttle_stop` action exists and is remappable (e.g. to `K`)
for users who want a dedicated key, but no key is spent on it by default.

On stop, the playhead **commits** to its current position by default
(`<Space>p` is the explicit return-to-origin variant). Configurable:

```lua
require("davimci.transport").configure({
  leader = "<Space>",
  play_pause = "<Space><Space>",   -- or e.g. "<C-Space>"
  on_stop = "commit",              -- or "return"
  shuttle_speeds = { 1, 2, 4, 8 },
  shuttle_back = "H",
  shuttle_forward = "L",
  shuttle_stop = nil,              -- unbound by default; e.g. "K"
})
```

All transport keys are remappable like any other binding. A user who wants
`<Space>` bare as play/pause simply loses it as leader and remaps.

Playing from the end of the timeline is refused with a reason rather than
reported as playback: the playhead may legally sit there (spec 15.2), there is
nothing after it, and a status line that says "playing" while nothing moves
is a lie. Playback that has run to the end must be startable again from
anywhere in bounds - reaching the end is a stop, never a state the editor has
to be restarted to leave.

#### Interrupting playback

A bind pressed during playback is **not** swallowed. Every action carries a
transport policy:

| Policy | Actions | Meaning |
|---|---|---|
| `interrupt` | motions, marks jumps, every edit, `u` / `Ctrl-r` / `.`, macro replay, media insert/append/replace | stop the clock first, then run |
| `keep` | the transport family itself, `zi`/`zo`/`z0`, mark set, macro record start/stop, mode changes (`v`, `:`, `Esc`), Lua callbacks | run without touching the clock |

An interrupt **commits** the playhead at the frame playback had reached, then
runs the action from there; it discards a pending `<Space>p` return-to-origin,
because the point of interrupting with a motion is to land where the motion
says. Interrupting is idempotent and silent when nothing is playing.

A `:` command line that edits interrupts unconditionally, before the command
runs.

The `interrupt_transport` action stops playback without running anything else.
It has no default binding; it exists so a user bind, a `:` mapping, or a Lua
callback can pause explicitly (`editor.interrupt_transport`, 9.9). A Lua
keymap callback defaults to `keep` and opts in per binding:

```lua
map("normal", "gh", my_handler, { interrupt = true })
```

### 3.3 Clip/edit-point motions

| Key | Action |
|---|---|
| `w` / `b` | next / previous clip boundary (current track) |
| `e` | end of current clip |
| `0` / `$` | start / end of timeline |
| `gg` / `G` | start / end of timeline (alias of above, vim muscle memory) |
| `{` / `}` | previous / next marker |
| `%` | jump to matching edit point (other side of a transition/cut) |
| `gt` / `gT` | next / previous track (cycle focus) |
| `` `a `` / `ma` | jump to mark "a" / set mark "a" at playhead |

### 3.4 Scripted / conditional motions

Motions can be **predicate-based**, defined in Lua and bound to keys or run
ad-hoc via command mode:

```
:goto next where track=A2 and rms_db > -2
```

or bound:

```lua
map("normal", "]a", motions.next_audio_peak({ track = "A2", threshold_db = -2 }))
```

Predicate motions are answered by the analysis index (spec 10.2), which is built
in the background. A query therefore has three outcomes, not two: a match, a
definite no-match, or **pending**. A partially analysed track always reports
pending - the playhead does not move and the status line says analysis is
still running, because a guessed landing frame is worse than none.

Built-in predicate motion library (extensible):
- `next_audio_peak(track, threshold_db)`
- `next_silence(track, min_duration_ms, threshold_db)`
- `next_scene_change(track)` (via frame-diff heuristic, optional ML hook)
- `next_clip_tagged(tag)`

---

## 4. Editing Verbs

Core verbs the user called out as most important: **move playhead, split at playhead, ripple delete.** These get the shortest/most ergonomic bindings.

| Key | Action |
|---|---|
| `s` | **Split** all clips at playhead on current track (or current selection scope) |
| `gs` | Split at playhead across **all** tracks (global split) |
| `x` | Ripple delete clip under playhead (current track) - shifts later clips left |
| `dw` | Ripple delete from playhead to next clip boundary |
| `d}` | Ripple delete from playhead to next marker |
| `dd` | Delete whole clip under playhead (ripple) |
| `gd` | **Lift** (delete, leave gap - non-ripple) instead of ripple delete |
| `y` + motion | Yank (copy) clip/range into a register |
| `p` / `P` | Paste (insert) after/before playhead, rippling later clips |
| `gp` / `gP` | Paste as **overwrite** (no ripple) |
| `c` + motion | Change: delete range and drop into `INSERT` to replace it |
| `r` | Replace clip under playhead with another media source |
| `i` | Insert media at playhead (opens media picker → drops at playhead, ripples) |
| `a` | Append media after current clip |
| `u` / `Ctrl-r` | Undo / redo |
| `.` | Repeat last edit verb (e.g. repeat a ripple-delete pattern across many cuts) |
| `q<reg>` / `@<reg>` | Record / replay macro |

### 4.0.1 Trim verbs

Trimming adjusts clip edges without splitting. The edge under consideration is
the one nearest the playhead on the current track unless a scope object says
otherwise.

| Key | Action |
|---|---|
| `t` + motion | **Ripple trim** the nearest edge to the motion target (later clips shift) |
| `gt` + motion | **Roll** edit: move the cut point, both adjacent clips absorb the change, total duration unchanged |
| `<` / `>` | Trim edge left / right by one jump point (count-prefixed); this is the ripple trim `t` performs, with the jump-point set deciding where the edge lands, so the step follows the zoom (spec 3.2). No jump point in that direction is a user error and trims nothing |
| `T` | Slip: shift a clip's source in/out points without moving it on the timeline |
| `gT` | Slide: move a clip along the timeline, adjacent clips absorb the change |

Note this reassigns `gt`/`gT` from section 3.3's track cycling; track cycling moves to
`]t` / `[t` to free the more valuable trim bindings.

### 4.1 `dW`-style scoping (the user's shorthand)

Interpreting the user's `dW ig?` shorthand: the intent is ripple-delete with a
**scope modifier**, similar to vim's `iw`/`aw` text objects. We generalize this
as **track-object modifiers**:

| Object | Meaning |
|---|---|
| `ic` | inner clip (just this clip's content) |
| `ac` | a clip including its adjoining transition/crossfade |
| `it` | inner track (whole current track only, ignore others at this time range) |
| `at` | a track-group (current track + its "linked" tracks, e.g. video+its audio) |
| `is` | inner segment (a sub-range within a clip, set via VISUAL) |

So:
- `dic` → ripple-delete just this clip's core content
- `dac` → ripple-delete clip + transition
- `dit` → ripple-delete on this track only, other tracks unaffected (gap stays on them unless ripple-linked)
- `dat` → ripple-delete across the linked track-group (e.g. video clip + its sync'd audio)

Every object resolves to a **(range, scope)** pair, where the scope is the set
of tracks the verb may touch:

| Object | Range | Scope |
|---|---|---|
| `ic` | the clip under the playhead, transitions excluded | focused track only |
| `ac` | the clip plus its adjoining transitions | focused track only |
| `it` | the clip's extent | focused track only, link groups ignored |
| `at` | the clip's extent | every track the clip's link group reaches; identical to `it` for an unlinked clip |
| `is` | the VISUAL selection; fails outside VISUAL | focused track only |

`ac` resolves to the same range as `ic` until a transition is attached to one
of the clip's cuts (spec 6.2); it then widens to cover the whole overlap, with no
change at the call site.

This directly answers the "edit single tracks at a time, or grouped tracks"
requirement: **the object you delete/select determines whether the operation
is track-scoped or group-scoped**, and grouping is a per-clip relationship
(see section 5).

---

## 5. Track Model & Grouping

- Track types: `video`, `audio`, `text/subtitle`, `overlay` (image/video composited above base video).
- Tracks are named by kind and index (`V1`, `A2`, `T1`). Names are unique, and
  a new track takes the **lowest free index** for its kind, so removing `V1`
  from a `V1`/`V2` stack makes the next video track `V1` again.
- Tracks can be **linked** into a group (e.g. camera video + its own audio). Operations default to respecting group linkage unless a scope modifier (`ic`/`it`) overrides it.
- Linkage is per-clip, not global - e.g. you can unlink one clip's audio from its video (`:unlink`) to trim just the talking without shifting video.
- A group's clips must stay frame-aligned: linking clips whose starts or ends differ is rejected. Operations that can no longer preserve alignment drop linkage rather than break it - splitting a clip leaves the right-hand half unlinked, and pasted or moved clips are always unlinked. Re-link with `:link`.
- Current-track focus (`j`/`k`) determines default operation scope in NORMAL mode.
- `VISUAL-BLOCK` mode lets you select a time range across an arbitrary chosen subset of tracks: enter with `Ctrl-v` (block-visual, vim-familiar), then toggle tracks in/out of the selection with `j`/`k` + `Space` (or a dedicated toggle key), independent of grouping. This satisfies "select only Video1 or Video2 or Audio2 even in a grouped clip."

---

## 6. Visual Mode Selection Granularity

`v` - visual (time-range) select on current track.
`V` - visual line - selects whole clip(s) fully, snapping to clip boundaries.
`Ctrl-v` - visual block - select a time range across multiple specific tracks.

Within visual mode:
- Motions (`h`/`l`/`w`/`b`/etc.) extend the selection as usual.
- `o` swaps the active end of the selection (standard vim behavior).
- Track objects (`it`/`at`) can still modify scope while in visual mode, e.g. select a time range, then `it` to constrain it to only the current track even if it started as a track-group selection. Typing an object in a VISUAL mode changes the selection's *scope* and never its range: `it` keeps the focused track, `at` keeps the focused track and everything its link group reaches.
- Once selected, apply a verb: `d` (ripple delete selection), `y` (yank), `gd` (lift, leave gap), `s` (split at both bounds and isolate segment).

---

## 6.1 Audio Operations

Audio is workflow step 3 and needs verbs beyond deletion - most "remove the
talking" cases are better served by muting or ducking than by cutting.

| Key / command | Action |
|---|---|
| `<Space>m` | Toggle mute on current track |
| `<Space>s` | Toggle solo on current track |

Solo is exclusive by *effect*, not by state: any soloed track silences every
track that is not soloed, so several tracks can be soloed at once and clearing
the last one restores normal playback. Mute and solo are track state, not
edits to clips, and they change only what the backend renders.
| `+` / `-` | Adjust gain on current clip or selection by a step (default 1 dB, count-prefixed) |
| `:gain <db>` | Set absolute gain on the current clip/selection |
| `:normalize [target_db]` | Normalize selection to a target loudness |
| `f` + motion | **Fade** across the motion range (fade in if at clip head, out if at tail) |
| `:fade in\|out <ms>` | Explicit fade with a duration |
| `:duck <track> <db>` | Duck this track wherever another track is above a threshold |

Audio lanes draw the analysed peak envelope of the source under each clip,
mapped through the clip's in-point so a trimmed clip shows its own audio. A
lane whose analysis has not landed draws nothing, since an empty lane and a
silent one must not look alike.

Gain and fades are **clip properties**, not destructive edits - they are stored
on the clip, apply as filters at render time, and are undoable like any other
command. Because they change the audio, they invalidate the analysis of that
track: the envelope is dropped and analysis re-runs in the background, so
predicate motions report `Pending` rather than a measurement of the pre-gain
signal (spec 10.2). `:analyze` forces the same thing by hand.

`:gain`, `:normalize`, `:fade` and `+`/`-` act on every clip the visual
selection overlaps, or on the clip under the playhead when nothing is
selected. A selection reaches a `:` line through the host seam: `:` leaves
visual mode, so the selection is the one that was live when `:` was pressed.
Whatever the scope, the whole set is one command, so one `u` undoes it. A
selection that overlaps no clip is refused rather than silently doing
nothing.

`:duck` lowers every part of the selected tracks - the playhead's track when
nothing is selected - that overlaps a region where the named track is above
the silence threshold, splitting the clips around those regions. It is one
command, so one `u` undoes the whole duck, and ducking a track against itself
is refused.

## 6.2 Transitions

Transitions occupy the overlap between two adjacent clips and are what `ac`
("a clip including its adjoining transition") refers to.

| Key / command | Action |
|---|---|
| `gx` | Create a default transition at the nearest cut (default: 12-frame crossfade / dissolve) |
| `:transition <name> [frames]` | Create a named transition at the nearest cut |
| `dax` | Delete the transition under the playhead |
| `:transition none` | The same deletion from command mode |

A transition belongs to the *incoming* clip: it is attached to the cut at that
clip's start, so deleting the clip deletes the transition with it, and an edit
that takes the cut away - a ripple over a neighbour, a trim that opens a gap -
resolves the transition rather than orphaning it.

The overlap is centred on the cut: for a `d`-frame transition the incoming
clip starts `d/2` frames early and the outgoing clip runs the remaining frames
past its out-point, with the odd frame going after the cut. Both come from
handle frames, so **nothing on the timeline moves**: no clip changes position
or duration, and the track length is identical before and after.

Creating a transition consumes handle frames from both clips; if either clip
lacks sufficient handles - or the overlap would be longer than the clips it
joins, or would run into the transition on a neighbouring cut - the operation
fails with a clear error rather than silently shortening the result.

Re-running `:transition` on a cut that already has one replaces it, which is
how a transition's type or duration is changed.

Transition types map onto MLT transitions: a video track composites, an audio
track always cross-fades whatever the type is named, and a type this build
does not know renders as a plain dissolve so that a project made elsewhere
still opens. Types are extensible from Lua.

---

## 7. File Format & Export Support

- **Import**: MKV as first-class citizen (multi-track audio and multi-subtitle streams inside a single MKV must be exposed as separate audio/text tracks on import, individually editable). Also support common containers (MP4, MOV, WebM) via the same import pipeline (likely FFmpeg-based demuxing).
- **Import is one edit.** Every track and clip a file produces is added by a
  single undoable command, so `u` after an import removes exactly what the
  import added - tracks included - and redo reproduces the same ids. An import
  into an empty project also sets the timeline properties (spec 7.1) as part of
  that same command.
Every audio track is exported as its own stream where the container allows it
(Matroska). Tracks are routed to their own channel range before they are
mixed, so this needs sources whose channel layout is known and no wider than
stereo, and at most eight of them; anything else exports as one mixed stream
and says so before the render starts rather than after it.

### 7.1 Timeline normalization (single framerate and resolution)

**The timeline has exactly one framerate and one resolution.** They are fixed
when the project is created (defaulting to the first imported clip's
properties, overridable at creation and via `:set timeline.fps` /
`:set timeline.resolution`).

Every imported source is **conformed** to the timeline on import:

- **Framerate**: sources at a different rate are retimed to the timeline rate.
  Integer-ratio cases (30 -> 60) duplicate frames; non-integer cases (23.976 ->
  30) use nearest-frame mapping by default, with optional blending configurable
  per-clip. The clip's timeline duration is always an exact whole number of
  timeline frames.
- **Resolution**: sources are scaled to fit the timeline frame, preserving
  aspect ratio, with configurable letterbox/pillarbox or crop-to-fill.
- **Audio**: resampled to the project sample rate.

This keeps the core `Frame(u64)` time model exact - there is one and only one
notion of "frame N" in a project, so splits, ripples, and marks can never drift
between tracks. Conforming is a *display and render* transformation; the
original source is untouched and export always relinks to it (spec 10.3).

Changing `timeline.fps` after clips exist re-conforms all clips and is a single
undoable command; the editor warns that frame-exact edit points may shift.

- **Multi-track audio**: each audio stream (whether from a multi-track MKV or separately imported files) becomes its own track - independently trimmable, mutable, ripple-delete-able, volume/gain-editable.
- **Export options**:
  - Configurable container/codec (MKV, MP4/H.264, MP4/H.265, WebM/VP9, ProRes for pro workflows)
  - Track selection at export (choose which audio tracks / subtitle tracks make it into the final render, or export multi-track MKV preserving separation)
  - Resolution/framerate presets, custom presets savable via config
  - `:render <preset>` / `:export <path> --preset <name>` command-mode invocation
  - `:presets` lists the presets that exist; `:cancel` stops a running export
  - An export is a background job: the editor stays usable while it runs, and
    its progress appears in the status line like any other job. A cancelled
    export leaves the partial file on disk - deleting a user's file is not the
    editor's decision.
  - `:export` with no `--preset` infers one from the output file's extension,
    and an output path with no extension takes the preset's.
  - Export presets definable in Lua config (see section 9)

---

## 8. Text/Subtitle & Overlay Layers

- Subtitles are a track type (`text`), each entry is a clip with a text payload + style.
- Editing a subtitle clip's text: playhead on the clip, `i` enters `INSERT` scoped to text-edit (opens text field), `Esc` returns to `NORMAL`.
- Import subtitle files (SRT/ASS) as a text track; export burned-in or as sidecar files.
- Overlay tracks (image/video composited above base) behave like video tracks but with an implicit z-order (track stacking order = compositing order) and transform properties (position/scale/opacity) editable via a properties panel or command mode (`:set clip.scale 0.5`).

---

## 9. Configuration System

### 9.1 Location & structure

```
~/.config/davimci/
├── init.lua              -- entrypoint, like nvim's init.lua
├── keymaps.lua           -- optional split-out keybindings
├── motions/              -- user-defined custom motions
│   └── my_motions.lua
├── presets/
│   └── export.lua        -- export preset definitions
└── plugin/               -- optional third-party plugins dropped here
```

`init.lua` simply `require`s the others, same convention as Neovim.

### 9.2 Keymap API

```lua
local map = require("davimci.keymap").map

-- mode, lhs, rhs (rhs can be a string command or a Lua function)
map("normal", "s", "editor.split_at_playhead")
map("normal", "x", "editor.ripple_delete")
map("normal", "<leader>e", function()
  require("davimci.export").run("youtube_1080p")
end)

-- rebind arrow keys' frame-step behavior
map("normal", "<Left>",  "editor.step_frame(-1)")
map("normal", "<Right>", "editor.step_frame(1)")
```

Modes are named `normal`, `visual`, `visual-line`, `visual-block`, `insert`,
`command`. A string right-hand side must name one of the `editor.*` commands
in section 9.9; an unknown name is rejected when the config loads, not when the key
is first pressed.

`map` takes an optional fourth argument, an options table. `interrupt = true`
gives the binding the `interrupt` transport policy (spec 3.2.1), which is the way
a Lua callback that edits stops playback before it runs:

```lua
map("normal", "gh", function() ... end, { interrupt = true })
```

A string right-hand side takes the policy of the `editor.*` command it names,
so `{ interrupt = ... }` is only meaningful for a function.

### 9.3 Custom motions (predicate-based)

```lua
local motions = require("davimci.motions")

motions.register("next_loud_audio", function(ctx, opts)
  return ctx.timeline:find_next({
    track = opts.track,
    type = "audio",
    predicate = function(sample) return sample.rms_db > opts.threshold_db end,
  })
end)

map("normal", "]a", function()
  motions.run("next_loud_audio", { track = "A2", threshold_db = -2 })
end)
```

This directly supports the requested "jump to next audio above -2dB in audio track 2" scripting use case.

### 9.4 Custom text objects

```lua
local textobj = require("davimci.textobject")

textobj.register("c", { -- clip
  inner = function(clip) return clip.core_range end,
  around = function(clip) return clip.range_with_transitions end,
})
```

Users can define new objects (e.g. `is` for silence-detected segment) the same way.

A registered object is typeable exactly as a built-in one is: its first
character is the key, so `register("c", ...)` makes `dic` and `dac` verbs over
it. Config wins over defaults, as it does for keymaps, so registering a name a
built-in object already uses replaces that object. The verb runs through the
command layer with the range the object returned, so it undoes, repeats and
records like any other edit; an object that returns nothing is reported and
edits nothing.

### 9.5 Export presets

```lua
require("davimci.export").preset("youtube_1080p", {
  container = "mp4",
  video_codec = "h264",
  audio_codec = "aac",        -- optional, defaults to aac
  resolution = "1920x1080",   -- optional, defaults to the timeline's
  audio_tracks = "all",       -- or "none", or {"A1", "A3"}
  subtitle_tracks = "burned", -- or "sidecar", "embedded", "none", or {"S1"}
})
```

A preset names a codec (`h264`, `h265`, `vp9`, `prores`, `aac`, `opus`,
`flac`, `pcm`); the editor maps it to an ffmpeg encoder, so a preset never
spells an encoder name (spec 10.3). Container/codec pairings are validated where
the preset is *defined*, not where it runs: a misspelled container is a user
error and must be reported when the config loads rather than after a long
render.

### 9.6 Zoom / jump-point config

```lua
require("davimci.timeline").configure({
  jump_points = { "clip_bounds", "markers", "silence" },
  jump_point_density_per_zoom = {
    [1] = "clip_bounds_only",
    [4] = "clip_bounds+markers",
    [10] = "dense_subdivision",
  },
  frame_step_keys = { "<Left>", "<Right>" }, -- always frame-accurate, remappable
})
```

### 9.7 Project-local overrides

- `.davimci.lua` in a project directory, auto-loaded on open, for per-project export presets, track linkage defaults, etc. - same modelines/local-config pattern as nvim's project-local `.nvimrc`-style setups (loaded opt-in for safety).
- Opt-in means what it says: an untrusted `.davimci.lua` is not read, not
  compiled, and not run, and the user is told it was skipped. Trust is
  granted per file path.
- A trusted project-local file still runs sandboxed. It sees `math`,
  `string`, `table`, the usual pure builtins, and the `davimci.*` modules. It
  does not see `os`, `io`, `load`, `dofile`, or `loadfile`, and its `require`
  resolves `davimci.*` and nothing else. "I want this project's export presets"
  is not "I want this directory to run arbitrary commands".

### 9.8 Hooks / events

```lua
require("davimci.autocmd").on("SplitPerformed", function(event)
  -- e.g. auto-tag both resulting clips
end)

require("davimci.autocmd").on("BeforeExport", function(ctx)
  -- e.g. validate no muted tracks are accidentally included
end)
```

Event list (v1): `PlayheadMoved`, `SplitPerformed`, `ClipDeleted`, `ClipInserted`, `ModeChanged`, `BeforeExport`, `AfterExport`, `ProjectLoaded`.

Handlers run in registration order. `BeforeExport` is the only cancellable
event in v1: a handler refuses the export either by returning `false` (with
an optional message) or by raising an error, and in both cases the render
does not start and the remaining handlers do not run. A raised error also
disables that handler for the session; a `false` return is a deliberate veto
and leaves it in place.

### 9.9 What Lua may and may not do

Lua **asks, it never writes.** Every call that means "change something"
(`davimci.editor.*`, `davimci.export.run`, `davimci.media.import`,
`davimci.motions.run`) queues a request, and the editor runs it through the
same command layer a keystroke would. Plugin edits are therefore ordinary
undo-tree entries, repeatable with `.` and recordable in a macro; there is no
second write path into the timeline.

The `editor.*` commands bindable from a keymap or callable from a callback:

| Command | Meaning |
|---|---|
| `editor.split_at_playhead` | `s` |
| `editor.split_all_tracks` | `gs` |
| `editor.ripple_delete` | `x` |
| `editor.paste` / `editor.paste_before` | `p` / `P` |
| `editor.undo` / `editor.redo` / `editor.repeat` | `u` / `Ctrl-r` / `.` |
| `editor.step_frame(n)` | `n` frames, sign gives direction |
| `editor.step_jump_point(n)` | `n` jump points, sign gives direction |
| `editor.play_pause` | `<Space><Space>` |
| `editor.interrupt_transport` | stop playback, commit the playhead (spec 3.2.1) |
| `editor.message(text)` | status-line message |

A registered motion (spec 9.3) is a pure query: it receives a snapshot - the
playhead, the focused track, clip bounds, and analysis samples - and returns
a frame. It cannot move the playhead itself, and a query against a track
whose analysis has not finished reports "not yet" rather than a frame, the
same rule the built-in predicate motions follow (spec 3.4).

A user callback that throws is logged, disabled for the rest of the session,
and anything it queued before throwing is discarded, so a half-run handler
cannot half-edit the timeline. A config file that fails to load costs that
file only.

### 9.10 Custom transitions

```lua
require("davimci.transition").register("sparkle", {
  service = "frei0r.sparkle",  -- required: what the backend renders it with
  density = "3",               -- everything else is a backend property
})
```

A registered type is usable anywhere a built-in one is: `:transition sparkle`,
`:set transition.type sparkle`, and the project file, which stores the name.
The config names a backend *service*; it never learns what the backend is, and
a backend with no transition registry reports that once at load and renders
those types as dissolves.

A name this build does not know **degrades to a dissolve** rather than failing
the render, so a project made with a plugin still opens without it. Audio is
unaffected either way: overlapping audio always cross-fades, whatever the type
is called (spec 6.2).

---

## 10. Decisions

Resolutions for the previously-open architectural questions. Each records the choice, the reasoning, and the risk being accepted.

### 10.1 Engine: embed MLT (`libmlt`) as the render/preview backend

**Decision:** build v1 on MLT rather than a custom renderer or raw FFmpeg filter graphs.

**Why:**
- MLT's playlist/tractor model maps almost 1:1 onto our track/clip model, so split and ripple are cheap in-memory playlist mutations - no re-render.
- Frame-accurate seeking, multi-track compositing, A/V sync, and a real-time preview consumer (SDL) all come for free.
- Producers are FFmpeg-backed, so the MKV/multi-track import story in section 7 is already covered.
- Raw FFmpeg would mean hand-writing a compositor, sync layer, and preview clock before the first `s` keypress works.

**Constraint:** the timeline model is **engine-agnostic**. We own the clip, track, grouping, and undo data structures; MLT sits behind a narrow `RenderBackend` interface (seek, preview, render, probe). A custom renderer can replace it later without touching the editor core.

**Accepted risk:** MLT's documentation is thin and the API is C with manual refcounting; expect a hand-written safe wrapper layer and a test suite that exercises it.

**Preview is a frame pull, not an MLT window.** Audio goes to a realtime MLT audio consumer, which owns the master clock; video frames are lifted out as RGBA buffers and presented by davimci. MLT never opens a window of its own, because a window it owned could not be composited with davimci's overlays and could not be shared between the GUI and the TUI.

**Preview scaling** is a decode-time request, not a post-scale: a half- or quarter-resolution pull asks for that size at `mlt_frame_get_image()`, so scrubbing and the TUI's small preview are cheap by construction.

**Offline media renders as a visible placeholder** - a distinctly coloured card, never black, so a missing source can never be mistaken for a gap. The project stays editable (Phase 0 policy) and export stays blocked.

### 10.2 Detection: precompute on import, in a background job

**Decision:** all silence/peak/scene analysis is precomputed. No real-time analysis during scrubbing.

**How:**
- One analysis pass per source on import: peak + RMS waveform at a fixed hop (default 10 ms), silence spans, and optional scene-change keyframes.
- Results cached to a versioned sidecar at `.davimci/cache/<content_hash>.analysis`. Cache version bumps invalidate.
- Predicate motions (spec 3.4) become an indexed lookup (O(log n)), so `]a` is instant and correct even when zoomed fully out.
- The job runs in the background with progress in the status line. Editing is allowed immediately; predicate motions report `analysis pending` until the relevant range is ready.
- Re-analysis is user-triggered (`:analyze`) after gain/filter changes.

### 10.3 Proxies: automatic above a threshold, transparent at export

**Decision:** proxy generation is on by default but conditional, and runs in the same background job as section 10.2.

**Rule:** generate a proxy when the source is above 1080p, or uses a long-GOP / expensive-to-seek codec (H.265, 10-bit, HEVC screen captures). Below that threshold, decode the original directly.

**Format:** 540p (configurable) intra-only ProRes Proxy or DNxHR LB, matching the source framerate and timecode so frame numbers stay identical. ProRes Proxy is the `prores_ks` encoder at profile 0; `codec` names an ffmpeg encoder, not a marketing name.

**Controls:**

```lua
require("davimci.media").configure({
  proxy = { auto = true, height = 540, codec = "prores_ks" },
})
```

plus `:set proxy on|off` at runtime.

**Hard invariant:** export always relinks to original sources. A built-in `BeforeExport` check fails the render if any clip would resolve to a proxy.

### 10.4 Undo: operational command log with an undo tree

**Decision:** every edit is a serializable command object that applies to the timeline and reports the command that undoes it, recorded to a log. Not full-state snapshots.

The inverse is produced *by* applying, not derived from the command alone: the inverse of a ripple delete is "put these clips back", which is only known once the delete has run.

**Why one decision buys five features:** undo/redo, `.`-repeat, macros (`q`/`@`), the Lua scripting API surface, and the project file format all fall out of the same command representation. A snapshot model gives only undo.

**Shape:**
- Undo is a **tree**, not a stack - branching history is cheap here and fits the vim model (`u`, `Ctrl-r`, `g-`/`g+`, `:undolist`). `Ctrl-r` follows the most recently created branch; `g-`/`g+` step through every state in change order, across branches.
- Project file = a compacted timeline state plus the command log since it,
  **plus the undo tree itself**: reopening a project and pressing `u` steps
  back through what was done before it was saved, and `Ctrl-r` still follows
  the branches that existed then. Intermediate drift-guard snapshots are not
  saved - they are rebuilt on demand.
- **Redo is exact, ids included.** A logged command never mints an identifier the log does not record, so an edit that incidentally cuts a clip - inserting mid-clip, deleting a part-range - is recorded as an explicit split followed by the edit. Undoing it joins the cut back up, so undo of a whole-clip delete leaves no seam, while undo of a part-range delete correctly keeps the cut its two remaining halves need.
- **Drift guard:** a full state snapshot every N commands (default 100) and on every save, so undo cost is bounded and a buggy `invert` can never lose the project - only the commands since the last snapshot.

### 10.5 Naming

The project, config directory, Lua module namespace, and project-local file all use `davimci`: `~/.config/davimci/`, `require("davimci.*")`, `.davimci.lua`.

### 10.6 Still open

- GPU-accelerated preview path (MLT's OpenGL consumer vs. software) - defer until preview performance is measured on real footage.
- Whether text/subtitle rendering uses MLT's built-in producers or a custom layout engine for richer styling.

---

## 11. Default Keybinding Summary

This table is the summary. The complete list is `docs/keymap.md`, generated
from the keymap table itself so it cannot drift from what the editor is bound
to; `just docs` regenerates it.

| Key | Meaning |
|---|---|
| `h`/`l` | move playhead by relative jump point (zoom-aware) |
| `←`/`→` | move playhead by exactly one frame (fixed, remappable) |
| `j`/`k` | change current track focus |
| `w`/`b`/`e` | clip-boundary motions |
| `s` | split at playhead (current track scope) |
| `gs` | split at playhead (all tracks) |
| `x` | ripple delete clip at playhead |
| `d` + object (`ic`/`ac`/`it`/`at`/`is`) | scoped ripple delete |
| `gd` | lift (delete, leave gap) |
| `y`/`p` | yank / paste |
| `v`/`V`/`Ctrl-v` | visual / visual-line / visual-block (track) select |
| `ma`/`` `a `` | set / jump to mark |
| `q`/`@` | record / replay macro |
| `<Space><Space>` | play / pause |
| `H`/`L` | shuttle back / shuttle forward (stop is unbound; `<Space><Space>`) |
| `t`/`<`/`>`/`T` | ripple trim / trim edge / slip |
| `f` + motion | audio fade |
| `+`/`-` | gain adjust |
| `gx`/`dax` | create / delete transition at nearest cut |
| `zi`/`zo`/`z0` | zoom in / out / reset to default zoom (spec 15.2) |
| `:export`, `:render` | export via command mode |
| `:w`, `:q`, `:wq`, `:e` | project lifecycle (see section 12) |
| `]a`, `[a` (example) | scripted predicate motions (user-defined) |

---

## 12. Project Lifecycle & Buffers

Projects behave like vim buffers, with the same command vocabulary.

| Command | Action |
|---|---|
| `:w [path]` | Save project (compacted snapshot + command log, spec 10.4) |
| `:q` / `:q!` | Close project; `:q` refuses on unsaved changes |
| `:wq` / `:x` | Save and close |
| `:e <path>` | Open a project or import a media file into a new timeline |
| `:ls` | List open timelines |
| `:bn` / `:bp` / `:b <n>` | Switch between open timelines |
| `:new` | New empty timeline (prompts for fps/resolution, see section 7.1) |
| `:relink [old] <new>` | Point offline clips at media that moved |
| `:analyze` | Re-run analysis on the current project (spec 10.2) |
| `:set <property> <value>` | Change one property (see 12.1) |

- Multiple timelines may be open simultaneously; registers and marks are
  **global** across timelines, so a yank in one can be pasted into another. A
  mark carries its frame across; its focused track applies only in the
  timeline the mark was set in, since track identity is per-timeline.
- `:q` refuses while the timeline differs from what is on disk. "Differs" is
  a comparison against the saved point in the undo tree, not a sticky flag, so
  undoing back to the saved state makes the timeline clean again.
- `:e` decides what a file is by reading it, not by its extension: a davimci
  project opens as a project, anything else is imported as media (spec 7).
- Where a command takes a single path (`:e`, `:w`, `:wq`), the argument is the
  **rest of the line**, not one whitespace-delimited word, so a filename with
  spaces needs no quoting or escaping. `:relink`, which takes two paths, is
  the exception and splits on whitespace.
- Autosave writes the command log continuously to `.davimci/autosave/`, enabling
  crash recovery on next open. Autosave never overwrites the project file.
  Each open timeline gets one log, named after the project path so two
  projects with the same file name cannot share one. Each record carries the
  edge of the undo tree its command was applied at, so recovery rebuilds the
  tree and not a line: `g-`/`g+` reach the same branches after a crash as
  before it. A record torn in half by the crash is dropped, and everything
  before it still recovers. `:w` and a clean `:q`
  delete it: a surviving log means the session did not survive, and the next
  open of that project offers to replay it. A recovered timeline is ahead of
  the file on disk and is therefore unsaved. A log that will not replay is
  reported as corruption rather than partially applied.
- `:relink` with one argument repoints the clip under the playhead; with two
  it repoints every clip whose media path is `<old>`. It is one undoable
  command however many clips it touches, and the clips come back online only
  if the new path exists - otherwise they stay offline and export stays
  blocked (spec 0 offline-media policy).
- `ProjectLoaded` fires after project-local `.davimci.lua` evaluation (spec 9.7).

### 12.1 `:set`

One command over a typed property registry, not a family of special cases.
`:set <property> <value>`; an unknown property and an out-of-range value are
both user errors, rejected before anything mutates and reported in a sentence
naming the property.

| Property | Value | Acts on |
|---|---|---|
| `clip.x`, `clip.y` | pixels | Selection, else the clip under the playhead |
| `clip.scale` | `> 0`, at most `100` | as above |
| `clip.opacity` | `0` to `1` | as above |
| `clip.gain` | dB, `-96` to `24` | as above |
| `clip.fade_in`, `clip.fade_out` | milliseconds, clamped to the clip | as above |
| `transition.duration` | frames, `> 0` | The transition under the playhead, else the one on the nearest cut |
| `transition.type` | a transition name | as above |
| `timeline.fps` | `25`, `29.97` or `30000/1001` | The timeline (re-conform, spec 7.1) |
| `timeline.resolution` | `1920x1080` | as above |
| `preview` | `on`/`off` | The session's preview (spec 15.5) |

- Every setter but `preview` is one command, so a change across a selection is
  one `u`, and `.` repeats it. `:set preview` is a view setting and never
  enters the undo log.
- `:set clip.gain` and `:set clip.fade_in|fade_out` mean exactly what `:gain`
  and `:fade` mean; the two spellings are the same command.
- `:set transition.*` changes the transition that is there and fails when
  there is none, rather than creating one; `:transition` creates.
- `:set timeline.fps` and `:set timeline.resolution` re-conform, and undo
  restores the exact prior geometry rather than recomputing it (spec 7.1).
- `:set preview off` stops the transport and stops pulling frames, so a
  session with no display still edits, saves and exports.

---

## 13. Licensing

The project is open source and not commercial.

- **davimci is GPL-3.0.** This is the least restrictive option that is also
  unambiguously safe given the dependency graph, and it costs nothing here.
- **MLT** (`extra/mlt`, 7.40.0) is `LGPL-2.1-only`, so dynamic linking imposes
  no obligation beyond LGPL compliance; GPL-3.0 is compatible.
- **Constraint:** link `libmlt` dynamically, never statically, and never link
  or vendor `melt`/`melted` (GPL-2). Shell out to them if ever needed.
- FFmpeg reaches us transitively through MLT's modules. If a build ever enables
  GPL-licensed FFmpeg components, the GPL-3.0 choice already accommodates it.
- Any Lua config a user writes is their own work and is not a derivative work
  of davimci.

---

## 14. Performance Targets

Deliberately coarse; the only hard requirement:

- **1080p60 playback and editing must be smooth**, with preview scaling (spec 10.3,
  proxies) permitted to hit it on expensive sources.
- Editing operations (split, ripple delete, undo) should feel instant on a
  timeline of a few hundred clips.
- **A held key moves at a constant speed.** Key repeats arrive faster than a
  frame can be decoded, so one batch of input costs one seek and one
  reprojection however many repeats it contains: every repeat moves the
  playhead, and only the frame the user ends on is decoded. Holding `h` or
  `l` must never lag behind the keyboard and must never stall.
- Predicate motions are indexed lookups and must not scan (spec 10.2).

Anything beyond this is measured before it is optimized.

A session can be scripted as a file of keystrokes and assertions - the same
format the integration tests use - and replayed with `davimci --script
<file>`. A directive per line: `keys <keystring>`, `cmd <: line>`,
`tick [n]`, `dump timeline|view`, and `expect` for mode, playhead, track,
clip count, message, or a substring of the timeline or view. A failing
assertion names the line that made it, so a bug report and a regression test
are the same artefact.

---

## 15. Frontend Behaviour

The editor's view state is defined once and every frontend renders it; no
frontend decides any of the following for itself.

Text is laid out with room for the padding a frontend puts inside a text box:
a label sized to its glyphs alone loses its last character, which is how a
two-digit ruler number ends up drawn as one digit.

### 15.1 Status line

`-- MODE (scope) --`, where scope is the focused track's name in `NORMAL`,
`INSERT` and `COMMAND`, and the comma-joined list of selected tracks in a
`VISUAL*` mode - e.g. `-- VISUAL (V1,A2) --` (spec 2). A running background job,
an active macro recording, and the most recent message follow it, in that
order.

### 15.2 Viewport

- One zoom level per timeline; it drives the jump-point set (spec 3.2) *and* the
  horizontal scale, so `h`/`l` and the ruler can never disagree.
- **Scroll-follow:** after any motion, the playhead and the focused track are
  visible. Following the playhead outranks staying inside the timeline, since
  the playhead may legally sit at the timeline's end.
- **Zoom anchors on the playhead:** the playhead keeps its screen column
  across a zoom step.
- **Zoom keys:** `zi` in, `zo` out, `z0` back to the default level. Zoom is
  view state, not an edit: it never enters the undo log, and a pointer wheel
  or a menu drives the same path.
- **Fit on first import:** importing media into an empty timeline picks the
  finest zoom level at which the whole new duration fits in the viewport
  width, and scrolls to frame 0. This applies only when the timeline was
  empty beforehand - a later import never moves the user's view - and, like
  every zoom, is view state rather than an edit.

- **Click to seek:** a click in the ruler or a lane moves the playhead to
  that column, and a click in a lane also focuses that track. A click is
  navigation, so it interrupts playback, never enters the undo log, and is
  decided above the frontends - a frontend reports where the click landed and
  nothing else.

- **Clip labels are readable.** A clip's label is drawn over whatever fills
  its lane - envelope, thumbnail or plain colour - never under it.

- **Thumbnails:** a video clip is drawn as a **filmstrip** - a picture every
  thumbnail-width across the clip, each of the media *at that point*, so a
  strip shows the shot changing rather than one frame stamped repeatedly.
  Sample points are anchored to the clip's start, not to the screen, so
  scrolling slides the strip instead of re-cutting it. The strip never
  crosses the clip's edge: a tile cut off there is cropped, never squashed
  into what is left.

  A frontend reports how wide it draws one thumbnail, since that depends on
  lane height and aspect; *which* frames are sampled is decided above the
  frontends, like everything else in the view. Pictures are decoded by the
  host: the app asks for the samples it would draw, nearest the playhead
  first, and the host decodes what it can afford - never while the transport
  is running, since the preview needs the decoder more. A picture is
  identified by the source frame it shows, so a slip or a trim leaves the
  strip to refill rather than showing the wrong frames, and a clip with no
  pictures yet is drawn plain rather than black.

### 15.3 Command line

- `:` opens it and it owns the keyboard until the line is submitted or
  cancelled. Esc cancels; backspacing over the leading `:` also cancels.
- The buffer, the caret position and the history live in the view state, not
  in a frontend: the line is **drawn as it is typed**, with a caret at the
  cursor, and two frontends cannot show two different lines.
- History is per session, deduplicated only against the immediately previous
  entry, browsed with Up/Down.
- Tab completes the word under the cursor to the *longest common prefix* of
  the matches, and never guesses between two commands.
- The candidates for the word under the cursor are **shown** while typing, on
  their own row above the line. A single candidate identical to what is
  already typed is not shown, and a list too long for the row is truncated
  with a count of what was left out. The vocabulary comes from the host, so
  completion covers exactly the commands that exist.

### 15.4 Media picker and text editing

- `i` / `a` / `r` open a media picker. It filters case-insensitively on the
  entry name, wraps at both ends, and descends into directories. The picker
  reads nothing itself: entries come from the host.
- INSERT mode on a subtitle clip edits text in a buffer. Esc commits it as an
  ordinary undoable command; an edit that ends equal to the original text
  commits nothing at all.

### 15.5 Video preview

- Audio is the master clock. Video is fitted to it: when several decoded
  frames are waiting, the ones the clock has already passed are **dropped**
  and only the newest is shown, and a tick with nothing ready **repeats** the
  last frame rather than going black. Dropping is a skip *towards* the clock,
  never below it: the newest frame that is not in the future is always shown,
  even when the clock has already passed it, and a frame pulled before it is
  due waits for the tick it falls due on rather than being shown early. The
  picture never steps backwards.
- Preview stills are cached, and a step backwards decodes the run leading up
  to its target in one pass, so walking backwards frame by frame costs one
  decode per frame rather than one seek per frame. Any change to the timeline
  invalidates the cache.
- The playhead follows the audio clock only once that clock has reached the
  frame playback started from. An audio consumer reports its pre-roll
  position until its first frame is shown, so before the clock locks the
  playhead stays put rather than flashing to the start of the timeline.
- The image is letterboxed - never stretched, never cropped - and centred on
  integral pixel boundaries.
- Overlays (timecode, safe areas) exist only in the embedded host. The
  detached preview window used with a terminal frontend is bare, undecorated
  and non-focusable, so the terminal keeps keyboard focus.
- Timecode is `HH:MM:SS:FF` at the timeline's nominal rate; there is no
  drop-frame representation, because the model is whole frames at one rate
  (spec 7.1).

### 15.6 Terminal frontend

Optional, and started with `davimci --tui` from a build with the `tui`
feature. It renders the same view state the window does, so every keybinding,
mode, message and `:` command behaves identically; only the drawing differs.

- The screen is a ruler row, one row per visible track, a status line, and the
  `:` line when it is open. Opening the `:` line takes its rows from the
  tracks, so nothing is ever drawn over it.
- Each track row is a name gutter of ten cells followed by one cell per
  timeline column. Ruler ticks are `┼` at a clip boundary and `┬` at a
  subdivision; the playhead is `▼` on the ruler and `│` on the focused track.
  A clip is drawn as a filled band with its label, an offline clip as a
  hatched one, and an audio clip carries its envelope as block characters.
- A left click seeks to the column under it, and picks up the track under it
  unless it landed on the ruler - the same rule the window follows.
- Modals - the media picker and subtitle editing - take the track rows while
  they are open and give them back on close, since a terminal has no floating
  window.
- Preview is a detached window (spec 15.5), and `:set preview off` runs a
  session with no display at all.
- What a terminal does not have: in-video overlays, a properties panel, clip
  filmstrips, and any timeline resolution finer than one cell per column.
