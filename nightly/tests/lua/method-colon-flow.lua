-- Method call sugar. `obj:pipe(x)` passes obj as an implicit self plus x. The
-- wrapper stores the tainted argument on itself and hands it back, exercising the
-- colon-call desugaring end to end.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local Wrapper = {}
Wrapper.__index = Wrapper

function Wrapper.new()
  return setmetatable({ payload = nil }, Wrapper)
end

function Wrapper:store(x)
  self.payload = x
  return self
end

function Wrapper:reveal()
  return self.payload
end

local function main()
  local w = Wrapper.new()
  w:store(source())
  sink(w:reveal())
end

main()
