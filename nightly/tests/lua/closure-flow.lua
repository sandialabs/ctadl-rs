-- Closures capturing an upvalue. make_getter closes over a tainted local, and
-- calling the returned closure surfaces the taint at the sink.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function make_getter(value)
  return function()
    return value
  end
end

local function main()
  local getter = make_getter(source())
  local leaked = getter()
  sink(leaked)
end

main()
