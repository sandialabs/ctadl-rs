-- Cross-module flow selected by fully-qualified id. `lib.reader.read` and
-- `lib.decoy.read` share the bare name `read`, so `names: ["read"]` would model
-- both; this case's model uses `qualified-id` to pick out exactly one.

local reader = require "lib.reader"
local decoy = require "lib.decoy"
local read = reader.read

local function sink(x)
  print(x)
end

local function main()
  local tainted = read()
  sink(tainted)
  -- `lib.decoy.read` is not modeled as a source; a flow reported here means the
  -- query's bare-name probe `read` matched, i.e. ids were keyed on bare names.
  sink(decoy.read())
end

main()
