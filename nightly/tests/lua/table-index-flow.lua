-- Bracket indexing with a string key. t["k"] = source() must reach sink(t["k"]).

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local t = {}
  t["payload"] = source()
  t["other"] = "clean"
  sink(t["payload"])
end

main()
