-- which-key: show what can follow a half-typed key sequence.
--
-- A view of the grammar, not a copy of it: the editor reports the pending
-- keys and every continuation the keymap allows, and this only lays them
-- out. The panel never takes focus, so it cannot swallow the key it is
-- waiting for.

local ui = require("davimci.ui")
local autocmd = require("davimci.autocmd")

local panel = ui.panel({
  title = "which-key",
  anchor = "bottom-left",
  z = 10,
})
panel:hide()

local function line(entry)
  return {
    { text = entry.key, role = "key" },
    { text = "  " .. (entry.group and "+prefix" or entry.description) },
  }
end

autocmd.on("KeyPending", function(e)
  if e.keys == "" or #e.continuations == 0 then
    panel:hide()
    return
  end
  local lines = { { { text = e.keys, role = "accent" } } }
  for _, entry in ipairs(e.continuations) do
    lines[#lines + 1] = line(entry)
  end
  panel:set_lines(lines)
  panel:show()
end)
