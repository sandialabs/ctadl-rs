-- Multiple assignment. split() returns a tainted value and a clean one; only the
-- tainted binding may reach a sink. The clean binding's sink is an unexpected line.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function split()
  return source(), "constant"
end

local function main()
  local tainted, clean = split()
  sink(tainted)
  sink(clean)
end

main()
