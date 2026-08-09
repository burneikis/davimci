-- subtitles: editing text tracks.
--
-- The model carries a clip's text and `SetClipText` is the only way to write
-- it, because that is the write path. What is not core is the opinion that a
-- text track is worth a workflow of its own: jumping cue to cue, and `i`
-- meaning "edit this subtitle" instead of "insert media". That opinion lives
-- here, so a build with this plugin off has one meaning for `i` and no
-- subtitle-shaped keys.

local motions = require("davimci.motions")
local keymap = require("davimci.keymap")

local function cues(ctx, opts)
  local track = ctx.tracks[opts.track or ctx.track]
  if not track or track.kind ~= "text" then
    return {}
  end
  return track.clip_bounds or {}
end

motions.register("next_subtitle", function(ctx, opts)
  local from = opts.from or ctx.playhead
  for _, frame in ipairs(cues(ctx, opts)) do
    if frame > from then
      return frame
    end
  end
end)

motions.register("prev_subtitle", function(ctx, opts)
  local from = opts.from or ctx.playhead
  local found
  for _, frame in ipairs(cues(ctx, opts)) do
    if frame >= from then
      break
    end
    found = frame
  end
  return found
end)

-- `]c` / `[c`: cue to cue. `]t` is track focus and stays that.
keymap.map("normal", "]c", function()
  motions.run("next_subtitle", {})
end)

keymap.map("normal", "[c", function()
  motions.run("prev_subtitle", {})
end)
