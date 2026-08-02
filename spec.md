# Vim-Motion Video Editor - Spec

## 1. Overview

A keyboard-first, modal video editor for cutting down footage, trimming audio, compositing overlays, and adding text/subtitles - controlled with vim-style motions, verbs, and modes. Configured like Neovim: a `.config/vimci/init.lua` entrypoint with a Lua scripting API, remappable keys, and hookable events.

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
- Configurable: `jump_point_density`, and whether jump points snap to
  (clip bounds | markers | beat-detected audio peaks | silence boundaries).
- Density is **monotonic in zoom**: zooming in only ever adds points, never
  moves or removes one, so a landing spot never shifts under the user. Below a
  configurable zoom level there are no subdivisions at all and `h`/`l` are
  purely clip- and marker-level; above it, subdivision spacing halves per level
  down to one frame.
- Frame zero and the end of the timeline are always jump points.

### 3.2.1 Transport / playback

Playback is a first-class mode-independent action, not a motion. `<Space>` is
the **leader** key; pressing it twice is play/pause, so the most common action
is also the easiest to reach without spending a dedicated key.

| Key | Action |
|---|---|
| `<Space><Space>` | Play / pause toggle |
| `L` / `J` | Shuttle forward / backward; press repeatedly to increase speed (1x, 2x, 4x, 8x) |
| `K` | Stop shuttle (return to 1x paused) |
| `<Space>p` | Play from playhead, return playhead to origin on stop (preview-and-return) |
| `<Space>l` | Loop the current selection (or current clip in NORMAL) |

Uppercase `J`/`K`/`L` are deliberately the JKL shuttle familiar from other
NLEs; lowercase `j`/`k`/`l` keep their vim meanings (track focus, jump points).

On stop, the playhead **commits** to its current position by default
(`<Space>p` is the explicit return-to-origin variant). Configurable:

```lua
require("vimci.transport").configure({
  leader = "<Space>",
  play_pause = "<Space><Space>",   -- or e.g. "<C-Space>"
  on_stop = "commit",              -- or "return"
  shuttle_speeds = { 1, 2, 4, 8 },
})
```

All transport keys are remappable like any other binding. A user who wants
`<Space>` bare as play/pause simply loses it as leader and remaps.

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

Predicate motions are answered by the analysis index (§10.2), which is built
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
| `<` / `>` | Trim edge left / right by one jump point (count-prefixed) |
| `T` | Slip: shift a clip's source in/out points without moving it on the timeline |
| `gT` | Slide: move a clip along the timeline, adjacent clips absorb the change |

Note this reassigns `gt`/`gT` from §3.3's track cycling; track cycling moves to
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

Until transitions exist (§6.2), `ac` resolves to the same range as `ic`; it
widens automatically once a transition can be attached, with no change at the
call site.

This directly answers the "edit single tracks at a time, or grouped tracks"
requirement: **the object you delete/select determines whether the operation
is track-scoped or group-scoped**, and grouping is a per-clip relationship
(see §5).

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
- Track objects (`it`/`at`) can still modify scope while in visual mode, e.g. select a time range, then `it` to constrain it to only the current track even if it started as a track-group selection.
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

Gain and fades are **clip properties**, not destructive edits - they are stored
on the clip, apply as filters at render time, and are undoable like any other
command. Because they change the audio, they invalidate the analysis cache for
that clip and require a re-run of `:analyze` for accurate predicate motions
(spec §10.2).

## 6.2 Transitions

Transitions occupy the overlap between two adjacent clips and are what `ac`
("a clip including its adjoining transition") refers to.

| Key / command | Action |
|---|---|
| `gx` | Create a default transition at the nearest cut (default: 12-frame crossfade / dissolve) |
| `:transition <name> [duration]` | Create a named transition at the nearest cut |
| `dax` | Delete the transition at the nearest cut |
| `:set transition.duration <frames>` | Adjust the transition under the playhead |

Creating a transition consumes handle frames from both clips; if either clip
lacks sufficient handles, the operation fails with a clear error rather than
silently shortening the result. Transition types are extensible from Lua and
map onto MLT transitions.

---

## 7. File Format & Export Support

- **Import**: MKV as first-class citizen (multi-track audio and multi-subtitle streams inside a single MKV must be exposed as separate audio/text tracks on import, individually editable). Also support common containers (MP4, MOV, WebM) via the same import pipeline (likely FFmpeg-based demuxing).
- **Import is one edit.** Every track and clip a file produces is added by a
  single undoable command, so `u` after an import removes exactly what the
  import added - tracks included - and redo reproduces the same ids. An import
  into an empty project also sets the timeline properties (§7.1) as part of
  that same command.
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
original source is untouched and export always relinks to it (spec §10.3).

Changing `timeline.fps` after clips exist re-conforms all clips and is a single
undoable command; the editor warns that frame-exact edit points may shift.

- **Multi-track audio**: each audio stream (whether from a multi-track MKV or separately imported files) becomes its own track - independently trimmable, mutable, ripple-delete-able, volume/gain-editable.
- **Export options**:
  - Configurable container/codec (MKV, MP4/H.264, MP4/H.265, WebM/VP9, ProRes for pro workflows)
  - Track selection at export (choose which audio tracks / subtitle tracks make it into the final render, or export multi-track MKV preserving separation)
  - Resolution/framerate presets, custom presets savable via config
  - `:render <preset>` / `:export <path> --preset <name>` command-mode invocation
  - Export presets definable in Lua config (see §9)

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
~/.config/vimci/
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
local map = require("vimci.keymap").map

-- mode, lhs, rhs (rhs can be a string command or a Lua function)
map("normal", "s", "editor.split_at_playhead")
map("normal", "x", "editor.ripple_delete")
map("normal", "<leader>e", function()
  require("vimci.export").run("youtube_1080p")
end)

-- rebind arrow keys' frame-step behavior
map("normal", "<Left>",  "editor.step_frame(-1)")
map("normal", "<Right>", "editor.step_frame(1)")
```

### 9.3 Custom motions (predicate-based)

```lua
local motions = require("vimci.motions")

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
local textobj = require("vimci.textobject")

textobj.register("c", { -- clip
  inner = function(clip) return clip.core_range end,
  around = function(clip) return clip.range_with_transitions end,
})
```

Users can define new objects (e.g. `is` for silence-detected segment) the same way.

### 9.5 Export presets

```lua
require("vimci.export").preset("youtube_1080p", {
  container = "mp4",
  video_codec = "h264",
  resolution = "1920x1080",
  audio_tracks = "all",       -- or {"A1", "A3"}
  subtitle_tracks = "burned", -- or "sidecar", or {"S1"}
})
```

### 9.6 Zoom / jump-point config

```lua
require("vimci.timeline").configure({
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

- `.vimci.lua` in a project directory, auto-loaded on open, for per-project export presets, track linkage defaults, etc. - same modelines/local-config pattern as nvim's project-local `.nvimrc`-style setups (loaded opt-in for safety).

### 9.8 Hooks / events

```lua
require("vimci.autocmd").on("SplitPerformed", function(event)
  -- e.g. auto-tag both resulting clips
end)

require("vimci.autocmd").on("BeforeExport", function(ctx)
  -- e.g. validate no muted tracks are accidentally included
end)
```

Event list (v1): `PlayheadMoved`, `SplitPerformed`, `ClipDeleted`, `ClipInserted`, `ModeChanged`, `BeforeExport`, `AfterExport`, `ProjectLoaded`.

---

## 10. Decisions

Resolutions for the previously-open architectural questions. Each records the choice, the reasoning, and the risk being accepted.

### 10.1 Engine: embed MLT (`libmlt`) as the render/preview backend

**Decision:** build v1 on MLT rather than a custom renderer or raw FFmpeg filter graphs.

**Why:**
- MLT's playlist/tractor model maps almost 1:1 onto our track/clip model, so split and ripple are cheap in-memory playlist mutations - no re-render.
- Frame-accurate seeking, multi-track compositing, A/V sync, and a real-time preview consumer (SDL) all come for free.
- Producers are FFmpeg-backed, so the MKV/multi-track import story in §7 is already covered.
- Raw FFmpeg would mean hand-writing a compositor, sync layer, and preview clock before the first `s` keypress works.

**Constraint:** the timeline model is **engine-agnostic**. We own the clip, track, grouping, and undo data structures; MLT sits behind a narrow `RenderBackend` interface (seek, preview, render, probe). A custom renderer can replace it later without touching the editor core.

**Accepted risk:** MLT's documentation is thin and the API is C with manual refcounting; expect a hand-written safe wrapper layer and a test suite that exercises it.

**Preview is a frame pull, not an MLT window.** Audio goes to a realtime MLT audio consumer, which owns the master clock; video frames are lifted out as RGBA buffers and presented by vimci. MLT never opens a window of its own, because a window it owned could not be composited with vimci's overlays and could not be shared between the GUI and the TUI.

**Preview scaling** is a decode-time request, not a post-scale: a half- or quarter-resolution pull asks for that size at `mlt_frame_get_image()`, so scrubbing and the TUI's small preview are cheap by construction.

**Offline media renders as a visible placeholder** - a distinctly coloured card, never black, so a missing source can never be mistaken for a gap. The project stays editable (Phase 0 policy) and export stays blocked.

### 10.2 Detection: precompute on import, in a background job

**Decision:** all silence/peak/scene analysis is precomputed. No real-time analysis during scrubbing.

**How:**
- One analysis pass per source on import: peak + RMS waveform at a fixed hop (default 10 ms), silence spans, and optional scene-change keyframes.
- Results cached to a versioned sidecar at `.vimci/cache/<content_hash>.analysis`. Cache version bumps invalidate.
- Predicate motions (§3.4) become an indexed lookup (O(log n)), so `]a` is instant and correct even when zoomed fully out.
- The job runs in the background with progress in the status line. Editing is allowed immediately; predicate motions report `analysis pending` until the relevant range is ready.
- Re-analysis is user-triggered (`:analyze`) after gain/filter changes.

### 10.3 Proxies: automatic above a threshold, transparent at export

**Decision:** proxy generation is on by default but conditional, and runs in the same background job as §10.2.

**Rule:** generate a proxy when the source is above 1080p, or uses a long-GOP / expensive-to-seek codec (H.265, 10-bit, HEVC screen captures). Below that threshold, decode the original directly.

**Format:** 540p (configurable) intra-only ProRes Proxy or DNxHR LB, matching the source framerate and timecode so frame numbers stay identical. ProRes Proxy is the `prores_ks` encoder at profile 0; `codec` names an ffmpeg encoder, not a marketing name.

**Controls:**

```lua
require("vimci.media").configure({
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
- Project file = a compacted timeline state plus the command log since it.
- **Redo is exact, ids included.** A logged command never mints an identifier the log does not record, so an edit that incidentally cuts a clip - inserting mid-clip, deleting a part-range - is recorded as an explicit split followed by the edit. Undoing it joins the cut back up, so undo of a whole-clip delete leaves no seam, while undo of a part-range delete correctly keeps the cut its two remaining halves need.
- **Drift guard:** a full state snapshot every N commands (default 100) and on every save, so undo cost is bounded and a buggy `invert` can never lose the project - only the commands since the last snapshot.

### 10.5 Naming

The project, config directory, Lua module namespace, and project-local file all use `vimci`: `~/.config/vimci/`, `require("vimci.*")`, `.vimci.lua`.

### 10.6 Still open

- GPU-accelerated preview path (MLT's OpenGL consumer vs. software) - defer until preview performance is measured on real footage.
- Whether text/subtitle rendering uses MLT's built-in producers or a custom layout engine for richer styling.

---

## 11. Default Keybinding Summary

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
| `J`/`K`/`L` | shuttle back / stop / shuttle forward |
| `t`/`<`/`>`/`T` | ripple trim / trim edge / slip |
| `f` + motion | audio fade |
| `+`/`-` | gain adjust |
| `gx` | create transition at nearest cut |
| `:export`, `:render` | export via command mode |
| `:w`, `:q`, `:wq`, `:e` | project lifecycle (see §12) |
| `]a`, `[a` (example) | scripted predicate motions (user-defined) |

---

## 12. Project Lifecycle & Buffers

Projects behave like vim buffers, with the same command vocabulary.

| Command | Action |
|---|---|
| `:w [path]` | Save project (compacted snapshot + command log, spec §10.4) |
| `:q` / `:q!` | Close project; `:q` refuses on unsaved changes |
| `:wq` / `:x` | Save and close |
| `:e <path>` | Open a project or import a media file into a new timeline |
| `:ls` | List open timelines |
| `:bn` / `:bp` / `:b <n>` | Switch between open timelines |
| `:new` | New empty timeline (prompts for fps/resolution, see §7.1) |
| `:analyze` | Re-run analysis on the current project (§10.2) |

- Multiple timelines may be open simultaneously; registers and marks are
  **global** across timelines, so a yank in one can be pasted into another.
- Autosave writes the command log continuously to `.vimci/autosave/`, enabling
  crash recovery on next open. Autosave never overwrites the project file.
- `ProjectLoaded` fires after project-local `.vimci.lua` evaluation (§9.7).

---

## 13. Licensing

The project is open source and not commercial.

- **vimci is GPL-3.0.** This is the least restrictive option that is also
  unambiguously safe given the dependency graph, and it costs nothing here.
- **MLT** (`extra/mlt`, 7.40.0) is `LGPL-2.1-only`, so dynamic linking imposes
  no obligation beyond LGPL compliance; GPL-3.0 is compatible.
- **Constraint:** link `libmlt` dynamically, never statically, and never link
  or vendor `melt`/`melted` (GPL-2). Shell out to them if ever needed.
- FFmpeg reaches us transitively through MLT's modules. If a build ever enables
  GPL-licensed FFmpeg components, the GPL-3.0 choice already accommodates it.
- Any Lua config a user writes is their own work and is not a derivative work
  of vimci.

---

## 14. Performance Targets

Deliberately coarse; the only hard requirement:

- **1080p60 playback and editing must be smooth**, with preview scaling (§10.3,
  proxies) permitted to hit it on expensive sources.
- Editing operations (split, ripple delete, undo) should feel instant on a
  timeline of a few hundred clips.
- Predicate motions are indexed lookups and must not scan (§10.2).

Anything beyond this is measured before it is optimized.
