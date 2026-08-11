-- silence: jump by silence, on the threshold you choose.
--
-- The editor can measure loudness; what counts as silence is an opinion, so it
-- lives here. `find_next` walks the analysis hops and this only decides
-- which hop is quiet enough, which is why changing the threshold needs no
-- new build.

local motions = require("davimci.motions")
local keymap = require("davimci.keymap")
local analysis = require("davimci.analysis")

-- Jumping by silence reads the loudness hops, and nothing is
-- measured unasked, so the plugin that wants them is what asks.
analysis.demand("silence")

local M = { threshold_db = -40 }

local function edge(ctx, opts, direction)
  local threshold = opts.threshold_db or M.threshold_db
  return ctx.timeline.find_next({
    track = opts.track or ctx.track,
    direction = direction,
    predicate = function(sample)
      return sample.rms_db <= threshold
    end,
  })
end

motions.register("next_silence", function(ctx, opts)
  return edge(ctx, opts, "forward")
end)

motions.register("prev_silence", function(ctx, opts)
  return edge(ctx, opts, "backward")
end)

keymap.map("normal", "]s", function()
  motions.run("next_silence", {})
end)

keymap.map("normal", "[s", function()
  motions.run("prev_silence", {})
end)

return M
