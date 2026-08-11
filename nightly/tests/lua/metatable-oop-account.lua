-- OOP via metatables. Account is a "class": Account.new sets a metatable whose
-- __index points back at Account, so instances resolve :deposit and :balance
-- through the metatable. Taint stored on the instance field via a method must
-- reach the sink when read back through another method.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local Account = {}
Account.__index = Account

function Account.new()
  local self = setmetatable({}, Account)
  self.value = nil
  return self
end

function Account:deposit(amount)
  self.value = amount
end

function Account:balance()
  return self.value
end

local function main()
  local acct = Account.new()
  acct:deposit(source())
  sink(acct:balance())
end

main()
