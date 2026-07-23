-- Array-style table: a tainted element inserted into a sequence is later read
-- back by numeric index and forwarded to the sink.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local items = {}
  items[1] = "safe"
  table.insert(items, source())
  for _, item in ipairs(items) do
    sink(item)
  end
end

main()
