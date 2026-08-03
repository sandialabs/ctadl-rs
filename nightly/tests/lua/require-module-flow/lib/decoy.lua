-- Module `lib.decoy`: exports a `read` with the same bare name as
-- `lib.reader.read`, but returning a constant. Nothing may flow from here, so a
-- reported flow through this module's `read` means the two collapsed into one
-- symbol -- the exact defect fully-qualified naming exists to prevent.

local _M = {}

function _M.read()
  return "constant"
end

return _M
