-- D4b (shape 4): the same as `resolved-callee-source-flow`, with the sink one
-- frame further up.
--
-- The taint leaves the resolved callee on a return edge at the dispatch
-- instruction, and then leaves `run` on an ordinary return. The second hop is
-- what exercises the context obligation the first one acquired: the call string
-- the resolution carries names `main`'s call to `run`, so the ordinary return
-- through that very site discharges it.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function run(g)
  return g()
end

local function main()
  local h = function() return source() end
  local r = run(h)
  sink(r)
end

main()
