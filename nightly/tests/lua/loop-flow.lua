-- Loop-carried flow: a tainted value read inside a numeric for loop is
-- accumulated into a local that is sunk after the loop.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local buffer = ""
  for _ = 1, 3 do
    buffer = buffer .. source()
  end
  sink(buffer)
end

main()
