-- scenes: jump to the cuts the analysis found in the footage.
--
-- The detector is core, because decoding is; deciding that a detected change
-- is a place worth landing on is not, so the jump lives here. A track with
-- no detection reports nothing rather than guessing a frame.

local motions = require("davimci.motions")
local keymap = require("davimci.keymap")

local function changes(ctx, opts)
  local track = ctx.tracks[opts.track or ctx.track]
  return track and track.scene_changes or {}
end

motions.register("next_scene", function(ctx, opts)
  local from = opts.from or ctx.playhead
  for _, frame in ipairs(changes(ctx, opts)) do
    if frame > from then
      return frame
    end
  end
end)

motions.register("prev_scene", function(ctx, opts)
  local from = opts.from or ctx.playhead
  local found
  for _, frame in ipairs(changes(ctx, opts)) do
    if frame >= from then
      break
    end
    found = frame
  end
  return found
end)

keymap.map("normal", "]v", function()
  motions.run("next_scene", {})
end)

keymap.map("normal", "[v", function()
  motions.run("prev_scene", {})
end)
