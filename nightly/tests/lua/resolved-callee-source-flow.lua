-- D4b (shape 3): the taint is created *inside* the resolved callee and consumed
-- in the frame that holds the indirect call.
--
-- No summary of the callback describes this: the value it returns comes from a
-- modelled endpoint, which does not exist at index time, so the callback has no
-- formal-to-out-formal flow to instantiate. Resolving the call into a summary
-- instantiation therefore carries nothing across the site; the flow needs a
-- *return edge* at the dispatch instruction. `tests/c/funcptrcalleesource.c` is
-- the language-neutral twin.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function run(g)
  local r = g()
  sink(r)
end

local function main()
  local h = function() return source() end
  run(h)
end

main()
