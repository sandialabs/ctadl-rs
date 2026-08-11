-- Field sensitivity: taint stored in one table field must reach a read of the
-- same field, while a sibling field stays clean.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local record = {}
  record.secret = source()
  record.public = "harmless"
  sink(record.secret)
end

main()
