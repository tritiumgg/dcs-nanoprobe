-- What a position row costs in the hook state, per row, in µs, on the
-- units that exist now, by class (category, moving or halted): the getter in
-- a Lua loop with locals, the same under pcall, into the DLL's sink, and
-- driven from the DLL (`csweep`, each call under lua_pcall). Also the cost of
-- a call that does nothing in each shape, the rows in ID order against a
-- shuffled order, a frame's DCS calls, and the cross-state call in parts.
-- Each figure is the best of 7 passes less an empty loop's pass, or the
-- median and p99 of 400 calls. One class or part is measured a frame. Reads
-- only; leaves nothing behind.
-- states: hook
-- timeout: 600
local E = ...
local a = E.lib('agent').open()
local clock, csweep, sink = a.clock, a.csweep, a.sink
local PASSES = 7

local function best(fn)
  local b = math.huge
  for _ = 1, PASSES do
    local t0 = clock()
    fn()
    local t = (clock() - t0) * 1e6
    if t < b then
      b = t
    end
  end
  return b
end

-- net.dostring_in returns the text and whether the chunk ran.
local function server(source)
  local text, ok = net.dostring_in('server', source)
  if ok == false then
    error('server: ' .. tostring(text), 2)
  end
  return text
end

-- The unit IDs by class, read in the server state and sent over in pieces.
local names = server([[
local classes, order = {}, {}
local function add(name, id)
  local c = classes[name]
  if not c then
    c = {}
    classes[name] = c
    order[#order + 1] = name
  end
  c[#c + 1] = id
end
local KIND = { [0] = 'airplane', [1] = 'helicopter', [2] = 'ground', [3] = 'ship' }
for side = 0, 2 do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    for _, u in ipairs(g:getUnits() or {}) do
      if u:isExist() then
        local d = u:getDesc()
        local kind = KIND[type(d) == 'table' and d.category or -1] or 'other'
        local v = u:getVelocity()
        local moving = v.x * v.x + v.y * v.y + v.z * v.z > 0.25
        add(kind .. (moving and '_moving' or '_halted'), u.id_)
        add('units', u.id_)
      end
    end
  end
  for _, s in ipairs(coalition.getStaticObjects(side) or {}) do
    if s:isExist() then
      add('statics', s.id_)
    end
  end
end
table.sort(order)
__np_perf = classes
return table.concat(order, ',')
]])
local classes, order = {}, {}
for name in tostring(names):gmatch('[^,]+') do
  local ids = {}
  local from = 1
  while true do
    local text = server(string.format(
      'local c = __np_perf[%q] return table.concat(c, ",", %d, math.min(#c, %d))', name, from, from + 399))
    local got = 0
    for id in tostring(text):gmatch('[^,]+') do
      ids[#ids + 1] = tonumber(id)
      got = got + 1
    end
    if got < 400 then
      break
    end
    from = from + 400
  end
  classes[name] = ids
  order[#order + 1] = name
end
server('__np_perf = nil')

local getter = DCS.getUnitPositionByObjectId

local function measure(ids)
  local n = #ids
  local out = { rows = n }
  if n == 0 or type(getter) ~= 'function' then
    return out
  end
  local f = getter
  local empty = best(function()
    for i = 1, n do
      local _ = ids[i]
    end
  end)
  out.lua = (best(function()
    for i = 1, n do
      local _, _, _ = f(ids[i])
    end
  end) - empty) / n
  out.lua_pcall = (best(function()
    for i = 1, n do
      pcall(f, ids[i])
    end
  end) - empty) / n
  out.lua_sink = (best(function()
    for i = 1, n do
      sink(f(ids[i]))
    end
  end) - empty) / n
  out.csweep = best(function()
    csweep(f, ids, 3)
  end) / n
  out.csweep_bare = best(function()
    csweep(f, ids, 3, 1)
  end) / n
  out.csweep_rows = best(function()
    csweep(f, ids, 3, 2)
  end) / n
  local zero = 0
  for i = 1, n do
    local x = f(ids[i])
    if x == 0 or x == nil then
      zero = zero + 1
    end
  end
  out.first_zero = zero
  return out
end

-- A call that does nothing, in each shape; and the order of the rows.
local function overheads(ids)
  local n = #ids
  local out = { rows = n }
  if n == 0 then
    return out
  end
  local function nop() end
  local abs = math.abs
  local empty = best(function()
    for i = 1, n do
      local _ = ids[i]
    end
  end)
  local function per(fn)
    return (best(fn) - empty) / n
  end
  out.lua_function = per(function()
    for i = 1, n do
      nop(ids[i])
    end
  end)
  out.lua_function_pcall = per(function()
    for i = 1, n do
      pcall(nop, ids[i])
    end
  end)
  out.lua_function_csweep = best(function()
    csweep(nop, ids, 0, 1)
  end) / n
  out.c_function = per(function()
    for i = 1, n do
      abs(ids[i])
    end
  end)
  out.c_function_pcall = per(function()
    for i = 1, n do
      pcall(abs, ids[i])
    end
  end)
  out.c_function_csweep = best(function()
    csweep(abs, ids, 1, 1)
  end) / n
  out.sink_alone = per(function()
    for i = 1, n do
      sink(1, 2, 3)
    end
  end)
  if type(getter) == 'function' then
    local sorted, shuffled = {}, {}
    for i = 1, n do
      sorted[i], shuffled[i] = ids[i], ids[i]
    end
    table.sort(sorted)
    local seed = 12345
    for i = n, 2, -1 do
      seed = (seed * 1103515245 + 12345) % 2147483648
      local j = seed % i + 1
      shuffled[i], shuffled[j] = shuffled[j], shuffled[i]
    end
    out.getter_in_id_order = best(function()
      csweep(getter, sorted, 3, 1)
    end) / n
    out.getter_shuffled = best(function()
      csweep(getter, shuffled, 3, 1)
    end) / n
  end
  return out
end

local function stats(list)
  table.sort(list)
  return { median = list[math.floor(#list / 2)], p99 = list[math.floor(#list * 0.99)] }
end

-- A frame's calls, and the cross-state call with each chunk.
local function frame_parts()
  local out = {}
  local N = 1000
  local empty = best(function()
    for _ = 1, N do
    end
  end)
  local function per(fn)
    return (best(fn) - empty) / N
  end
  local real, model, pause = DCS.getRealTime, DCS.getModelTime, DCS.getPause
  out.get_real_time = per(function()
    for _ = 1, N do
      real()
    end
  end)
  out.get_model_time = per(function()
    for _ = 1, N do
      model()
    end
  end)
  out.get_pause = per(function()
    for _ = 1, N do
      pause()
    end
  end)
  out.agent_clock = per(function()
    for _ = 1, N do
      clock()
    end
  end)
  out.agent_wants = per(function()
    for _ = 1, N do
      a.wants(1)
    end
  end)
  out.pcall_closure = per(function()
    for _ = 1, N do
      pcall(function() end)
    end
  end)
  return out
end

local function xcall()
  local function times(chunk)
    local t = {}
    for i = 1, 400 do
      local t0 = clock()
      net.dostring_in('server', chunk)
      t[i] = (clock() - t0) * 1e6
    end
    return stats(t)
  end
  server('__np_noop = { pump = function() end } function __np_p() end')
  local out = {
    empty_string = times(''),
    return_only = times('return'),
    global_call = times('__np_p()'),
    field_call = times('__np_noop.pump()'),
    pump_string = times('local s = __np_noop if s and s.pump then s.pump() end'),
  }
  -- The call's cost by the chunk's length alone: a call and a comment.
  for _, length in ipairs({ 50, 100, 200, 400 }) do
    out['padded_' .. length] = times('__np_p()--' .. string.rep('x', length - 10))
  end
  server('__np_noop = nil __np_p = nil')
  return out
end

local result = { has_getter = type(getter), classes = {}, order = order }
local step = 0
return E.wait(function()
  step = step + 1
  local name = order[step]
  if name then
    result.classes[name] = measure(classes[name])
    return nil
  end
  if step == #order + 1 then
    result.overheads = overheads(classes.units or {})
    return nil
  end
  if step == #order + 2 then
    result.frame = frame_parts()
    return nil
  end
  result.xcall = xcall()
  a.sink_take()
  return result
end, 540, 'running')
