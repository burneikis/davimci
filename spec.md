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

This directly answers the "edit single tracks at a time, or grouped tracks"
requirement: **the object you delete/select determines whether the operation
is track-scoped or group-scoped**, and grouping is a per-clip relationship
(see §5).

---

## 5. Track Model & Grouping

- Track types: `video`, `audio`, `text/subtitle`, `overlay` (image/video composited above base video).
- Tracks can be **linked** into a group (e.g. camera video + its own audio). Operations default to respecting group linkage unless a scope modifier (`ic`/`it`) overrides it.
- Linkage is per-clip, not global - e.g. you can unlink one clip's audio from its video (`:unlink`) to trim just the talking without shifting video.
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

## 7. File Format & Export Support

- **Import**: MKV as first-class citizen (multi-track audio and multi-subtitle streams inside a single MKV must be exposed as separate audio/text tracks on import, individually editable). Also support common containers (MP4, MOV, WebM) via the same import pipeline (likely FFmpeg-based demuxing).
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
~/.config/vimvid/
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
local map = require("vimvid.keymap").map

-- mode, lhs, rhs (rhs can be a string command or a Lua function)
map("normal", "s", "editor.split_at_playhead")
map("normal", "x", "editor.ripple_delete")
map("normal", "<leader>e", function()
  require("vimvid.export").run("youtube_1080p")
end)

-- rebind arrow keys' frame-step behavior
map("normal", "<Left>",  "editor.step_frame(-1)")
map("normal", "<Right>", "editor.step_frame(1)")
```

### 9.3 Custom motions (predicate-based)

```lua
local motions = require("vimvid.motions")

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
local textobj = require("vimvid.textobject")

textobj.register("c", { -- clip
  inner = function(clip) return clip.core_range end,
  around = function(clip) return clip.range_with_transitions end,
})
```

Users can define new objects (e.g. `is` for silence-detected segment) the same way.

### 9.5 Export presets

```lua
require("vimvid.export").preset("youtube_1080p", {
  container = "mp4",
  video_codec = "h264",
  resolution = "1920x1080",
  audio_tracks = "all",       -- or {"A1", "A3"}
  subtitle_tracks = "burned", -- or "sidecar", or {"S1"}
})
```

### 9.6 Zoom / jump-point config

```lua
require("vimvid.timeline").configure({
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

- `.vimvid.lua` in a project directory, auto-loaded on open, for per-project export presets, track linkage defaults, etc. - same modelines/local-config pattern as nvim's project-local `.nvimrc`-style setups (loaded opt-in for safety).

### 9.8 Hooks / events

```lua
require("vimvid.autocmd").on("SplitPerformed", function(event)
  -- e.g. auto-tag both resulting clips
end)

require("vimvid.autocmd").on("BeforeExport", function(ctx)
  -- e.g. validate no muted tracks are accidentally included
end)
```

Event list (v1): `PlayheadMoved`, `SplitPerformed`, `ClipDeleted`, `ClipInserted`, `ModeChanged`, `BeforeExport`, `AfterExport`, `ProjectLoaded`.

---

## 10. Open Questions / Follow-ups

1. **Engine choice** - build on an existing NLE engine/library (e.g. an FFmpeg-based compositing pipeline, or embed something like MLT) vs. a custom renderer. Affects how fast ripple/split operations can preview live.
2. **Silence/peak detection** - real-time (as you scrub) vs. precomputed on import (waveform + RMS analysis pass). Precompute is simpler and enables the `]a`-style predicate motions instantly.
3. **Proxy/preview resolution** - for smooth scrubbing at high zoom on large MKV sources, will likely need proxy transcodes generated on import.
4. **Undo model** - operational-transform-style command log (fits `.`-repeat and macros naturally) vs. full state snapshots.

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
| `:export`, `:render` | export via command mode |
| `]a`, `[a` (example) | scripted predicate motions (user-defined) |
