-- Lua twin of tests/c/callsmear.c: sink_hit_* are real flows; sink_clean_* stay silent.
local function source() return io.read() end
local function source_buf() return io.read() end
local function sink_hit_operand(x) print(x) end
local function sink_clean_operand(x) print(x) end
local function sink_hit_return(x) print(x) end
local function sink_clean_return(x) print(x) end
local function sink_hit_sibling(x) print(x) end
local function sink_clean_sibling(x) print(x) end
local function sink_hit_retarg(x) print(x) end
local function sink_clean_retarg(x) print(x) end
local function sink_hit_field(x) print(x) end
local function sink_clean_field(x) print(x) end

-- Operands do not taint each other.
local function operand(m)
  local n = source()
  local c = n + m
  sink_hit_operand(c)
  sink_clean_operand(m)
end

-- Return does not taint fields the callee read.
local function rowsize(p) return p.width * 4 end
local function call_return(p)
  local w = p.width
  local r = rowsize(p) + source()
  sink_hit_return(r)
  sink_clean_return(w)
end

-- Saturation does not reach sibling fields.
local function sibling(im)
  im.buf = source_buf()
  sink_hit_sibling(im.buf)
  sink_clean_sibling(im.bps)
end

-- Tainted return does not taint the argument (source: wrap).
local function wrap(n) return n end
local function retarg(m)
  local t = wrap(m)
  sink_hit_retarg(t)
  sink_clean_retarg(m)
end

-- Tainted return does not taint the field read (source: width_of).
local function width_of(p) return p.width end
local function field(p)
  local w = p.width
  local s = width_of(p)
  sink_hit_field(s)
  sink_clean_field(w)
end

operand(7)
call_return({ width = 640 })
sibling({ bps = 8 })
retarg(7)
field({ width = 640 })
