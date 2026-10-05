-- Call targets crossing an indirect call, both ways, with closures. The same defect as
-- `test_cli_query_c_funcptr_through_indirect_call`, through another front end.
--   up:          `run_up` calls `factory`, a closure passed in, and calls what it returns.
--   down:        `run_down` hands a closure to `op`, itself a closure passed in.
--   down_formal: `run_down_formal` hands on its own parameter `f` the same way.
--   `sink_clean` is in a closure the factory builds but does not return.
-- See `a_call_target_crosses_an_indirect_call` in tests/lua_call_targets.rs.
local function source() return io.read() end
local function sink_up(x) print(x) end
local function sink_down(x) print(x) end
local function sink_down_formal(x) print(x) end
local function sink_clean(x) print(x) end

local function run_up(factory)
  local h = factory()
  h(source())
end

local function run_down(op)
  op(function(x) sink_down(x) end, source())
end

local function run_down_formal(op, f)
  op(f, source())
end

run_up(function()
  local unused = function(x) sink_clean(x) end
  return function(x) sink_up(x) end
end)
run_down(function(f, v) f(v) end)
run_down_formal(function(g, v) g(v) end, function(x) sink_down_formal(x) end)
