-- Module `lib.decoy`: exports a `read` with the same bare name as
-- `lib.reader.read`, returning a constant. It is not modeled as a source, so a
-- reported flow through it means the two ids collapsed into a bare-name match.
-- Held to 11 lines for the same reason as lib/reader.lua.
local _M = {}

function _M.read()
  return "constant"
end

return _M
