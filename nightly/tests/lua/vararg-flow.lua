-- Varargs: a tainted argument passed through `...` is unpacked and reaches the
-- sink via select().

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function forward(...)
  local first = select(1, ...)
  sink(first)
end

local function main()
  forward(source(), "trailing")
end

main()
