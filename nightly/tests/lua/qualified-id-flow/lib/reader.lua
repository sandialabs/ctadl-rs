-- Module `lib.reader`: reads untrusted input. Its `read` is deliberately named
-- the same as `lib.decoy.read`; only the fully-qualified id `lib.reader.read`
-- separates them. Held to 11 lines so nothing here can collide with the
-- main.lua lines this case asserts on -- SARIF start lines are pooled.
local _M = {}

function _M.read()
  return io.read()
end

return _M
