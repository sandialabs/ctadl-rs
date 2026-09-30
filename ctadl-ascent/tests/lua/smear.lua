-- The same three shapes as smear.c, through ctadl's Lua front end.
-- Only the sink_hit_* calls are real flows; every sink_clean_* must stay silent.
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

-- 1. One operand of an expression must not taint the other.
local function operand(m)
  local n = source()
  local c = n + m
  sink_hit_operand(c)      -- real: n -> c
  sink_clean_operand(m)    -- m never held anything from source()
end

-- 2. A call's return value must not flow back onto the field the callee read.
local function rowsize(p) return p.width * 4 end
local function call_return(p)
  local w = p.width        -- read BEFORE the call, from an untainted table
  local r = rowsize(p) + source()
  sink_hit_return(r)       -- real: source -> r
  sink_clean_return(w)     -- w is p.width, which nothing tainted
end

-- 3. A tainted field must not taint its sibling field.
local function sibling(im)
  im.buf = source_buf()
  sink_hit_sibling(im.buf)    -- real: source_buf -> im.buf
  sink_clean_sibling(im.bps)  -- bps is a different field of im
end

-- 4. A tainted return value must not taint the call's own argument
--    (source: wrap's return, declared in smear.json).
local function wrap(n) return n end
local function retarg(m)
  local t = wrap(m)
  sink_hit_retarg(t)       -- real: the return value
  sink_clean_retarg(m)     -- m is the input, not the output
end

-- 5. A tainted return value must not flow back onto the field the callee read
--    (source: width_of's return, declared in smear.json).
local function width_of(p) return p.width end
local function field(p)
  local w = p.width        -- read before the call, from an untainted table
  local s = width_of(p)
  sink_hit_field(s)        -- real: the return value
  sink_clean_field(w)      -- w is p.width, which nothing tainted
end

operand(7)
call_return({ width = 640 })
sibling({ bps = 8 })
retarg(7)
field({ width = 640 })
