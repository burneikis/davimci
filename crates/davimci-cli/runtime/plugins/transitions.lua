-- transitions: the video transition catalogue.
--
-- The backend knows one type, `dissolve`, because a project has to open in
-- any build. Everything with a shape is registered here, through the same
-- API a third-party transition uses: a `luma` driven by a soft-edged
-- geometry, which is how MLT itself expresses a wipe.

local transition = require("davimci.transition")

transition.register("wipe_left", { service = "luma", resource = "%luma01.pgm" })
transition.register("wipe_right", { service = "luma", resource = "%luma01.pgm", invert = "1" })
transition.register("wipe_up", { service = "luma", resource = "%luma03.pgm" })
transition.register("wipe_down", { service = "luma", resource = "%luma03.pgm", invert = "1" })
transition.register("iris", { service = "luma", resource = "%luma05.pgm" })
