-- Negative case: source() is called but its result is never connected to the
-- sink. The sink only ever sees an independent constant, so no flow may be
-- reported. expected_lines is empty to assert the absence of a source -> sink path.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local tainted = source()
  local clean = "constant"
  -- `tainted` is deliberately dropped here.
  sink(clean)
end

main()
