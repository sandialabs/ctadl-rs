-- Cross-module flow: the tainted value crosses a file boundary through a
-- `require`d module table, and the hoisted local alias `read` resolves to the
-- module function it names rather than to `lib.decoy.read`.

local reader = require "lib.reader"
local decoy = require "lib.decoy"
local read = reader.read

local function sink(x)
  print(x)
end

local function main()
  local tainted = read()
  sink(tainted)
  -- The decoy returns a constant; sinking it must not produce a flow.
  sink(decoy.read())
end

main()
