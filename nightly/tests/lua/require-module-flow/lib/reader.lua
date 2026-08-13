-- Module `lib.reader`: reads untrusted input and hands it back through the
-- module table. Its `read` is deliberately named the same as `lib.decoy.read`,
-- which must not be confused with it.

local _M = {}

local function source()
  return io.read()
end

function _M.read()
  return source()
end

return _M
