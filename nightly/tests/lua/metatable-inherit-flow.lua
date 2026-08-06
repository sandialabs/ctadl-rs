-- Inheritance through a chain of __index metatables. Derived inherits get_data
-- from Base by setting Base as Derived's __index. Taint set on a Derived instance
-- flows to the sink through the inherited method.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local Base = {}
Base.__index = Base

function Base:set_data(d)
  self.data = d
end

function Base:get_data()
  return self.data
end

local Derived = setmetatable({}, { __index = Base })
Derived.__index = Derived

function Derived.new()
  return setmetatable({}, Derived)
end

local function main()
  local obj = Derived.new()
  obj:set_data(source())
  sink(obj:get_data())
end

main()
