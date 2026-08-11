-- Wildcard port expansion: a sink port naming a whole table argument must catch
-- taint that arrives on a *field* of that table, without the model enumerating
-- field names. Lua's options-table calling convention makes this the common
-- shape. The sibling field stays clean.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local opts = {}
  opts.headers = source()
  opts.public = "harmless"
  sink(opts)
end

main()
