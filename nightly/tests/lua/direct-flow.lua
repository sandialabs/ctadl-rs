-- Direct flow: a tainted value from source() flows straight into sink().
-- source() -> x -> sink(x)

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local x = source()
  sink(x)
end

main()
