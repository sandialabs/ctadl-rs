-- Control flow: the tainted value is assigned on one branch of an if/else. The
-- join after the branch carries the taint to the sink.

local function source()
  return io.read()
end

local function sink(x)
  print(x)
end

local function main(flag)
  local value
  if flag then
    value = source()
  else
    value = "default"
  end
  sink(value)
end

main(true)
