-- Nested field access: taint buried under two levels of table fields
-- (outer.inner.value) must still be tracked to the sink.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local outer = { inner = {} }
  outer.inner.value = source()
  sink(outer.inner.value)
end

main()
