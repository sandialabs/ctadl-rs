-- D4 (shape 2): the callback arrives from a caller, and the flow is consumed
-- inside the frame that holds the indirect call.
--
-- The engine resolves the call and derives a contextual assignment for it, but
-- until the query engine learned to traverse a `context_assign` under its
-- calling context there was no rule that made one usable *where it sits* --
-- the only consumers lifted it back out to the caller. Moving the sink into
-- `run` was enough to lose the flow. `tests/c/funcptrcalleeframe.c` is the
-- language-neutral twin.
--
-- The callback is a closure value, not a named `local function`, so the call
-- lowers to a data-flow-resolved indirect call rather than a direct edge.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

-- The indirect call and the sink both live here; the callback comes from main.
local function run(f, v)
  local r = f(v)
  sink(r)
end

local function main()
  local h = function(x) return x end
  run(h, source())
end

main()
