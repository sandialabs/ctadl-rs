-- Exercises the SHIPPED Lua propagation defaults (`models/defaults/lua-index.jsonl`).
--
-- Unlike every other case here this one defines no `source`/`sink` of its own: the source is
-- the real `io.read` and the sink the real `os.execute`, and the step between them --
-- `string.format` -- has no body in this program. The flow exists only if
--   (a) the Lua frontend publishes called-but-undefined names in its VMT `externals` column,
--       so a model can name them at all, and
--   (b) the default file is loaded for a Lua import and models `format`.
-- Break either and this case goes quiet. Nothing else in the suite says so.
--
-- `format` is matched by its bare name, which is also why `s:format(...)` and
-- `string.format(...)` need only one generator between them.

local function build(name)
  return string.format("echo %s", name)
end

local function main()
  local name = io.read()
  local cmd = build(name)
  os.execute(cmd)
end

main()
