-- D4c: one dispatch site with two CHA targets, and the sink in the target the
-- search engine's `callee_by_site` map did *not* keep.
--
-- A Lua method call emits its whole CHA target set as `call` rows. The search
-- engine indexed them with a plain `insert`, so a site kept whichever row
-- loaded last and the call-entry edge followed one target only; the datalog
-- regime, which joins `call` as a relation, found both. Swapping which class
-- holds the sink used to flip the answer -- which is how the two regimes came
-- to disagree.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local A = {}
A.__index = A
function A.new() return setmetatable({}, A) end
function A:go(x) sink(x) end

local B = {}
B.__index = B
function B.new() return setmetatable({}, B) end
function B:go(x) print("b", x) end

-- `o` is a parameter, so `o:go(v)` dispatches over both `A:go` and `B:go`.
local function run(o, v)
  o:go(v)
end

local function main()
  run(A.new(), source())
  run(B.new(), "clean")
end

main()
