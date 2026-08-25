-- D4b (shape 5): the taint is passed as an argument *into* the resolved callee
-- and consumed by a sink inside it.
--
-- The mirror of `resolved-callee-source-flow`, and the same defect: the
-- callback has no summary describing this (it returns nothing), so no summary
-- instantiation at the dispatch site can carry the argument in. The flow needs
-- a call *entry* edge there. `tests/c/funcptrcalleesink.c` is the
-- language-neutral twin.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function run(g, v)
  g(v)
end

local function main()
  local h = function(x) sink(x) end
  run(h, source())
end

main()
