-- One call site, two possible callees: `h` holds one of two closures. The query has to enter
-- both. It used to keep a single callee per call site, so only one sink was reached.
-- See `a_call_with_two_possible_callees_enters_both` in tests/lua_call_targets.rs.
local function source() return io.read() end
local function sink_a(x) print(x) end
local function sink_b(x) print(x) end

local function run(flag)
  local h
  if flag then
    h = function(x) sink_a(x) end
  else
    h = function(x) sink_b(x) end
  end
  h(source())
end

run(os.time() > 0)
