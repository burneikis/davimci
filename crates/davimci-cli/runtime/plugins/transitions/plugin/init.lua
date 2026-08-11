-- transitions: the whole transition catalogue, and the keys that use it.
--
-- Nothing about a transition is core. The model holds an overlap and a name;
-- what that name looks like, and which keys create one, live here. With this
-- plugin off there is no `gx`, and a name a project brought with it renders
-- as a bare overlap rather than failing to open.
--
-- Every type is registered through the same API a third-party transition
-- uses: a `luma` driven by a soft-edged geometry, which is how MLT itself
-- expresses a cross-fade or a wipe.

local transition = require("davimci.transition")
local keymap = require("davimci.keymap")

-- A `luma` with no resource is a plain cross-fade. It is first because it is
-- what `gx` creates, not because the host knows the name.
transition.register("dissolve", { service = "luma" })

transition.register("wipe_left", { service = "luma", resource = "%luma01.pgm" })
transition.register("wipe_right", { service = "luma", resource = "%luma01.pgm", invert = "1" })
transition.register("wipe_up", { service = "luma", resource = "%luma03.pgm" })
transition.register("wipe_down", { service = "luma", resource = "%luma03.pgm", invert = "1" })
transition.register("iris", { service = "luma", resource = "%luma05.pgm" })

keymap.map("normal", "gx", 'editor.transition_create("dissolve")')
keymap.map("normal", "dax", "editor.transition_delete")
