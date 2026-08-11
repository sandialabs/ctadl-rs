-- Interprocedural flow through a passthrough helper.
-- The tainted value is threaded through identity() and returned before the sink.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function identity(v)
  return v
end

local function main()
  local tainted = source()
  local relayed = identity(tainted)
  sink(relayed)
end

main()
