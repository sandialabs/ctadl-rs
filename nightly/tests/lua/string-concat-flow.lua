-- String concatenation propagates taint: a tainted value concatenated into a
-- larger message keeps the message tainted at the sink.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main()
  local name = source()
  local message = "hello, " .. name .. "!"
  sink(message)
end

main()
