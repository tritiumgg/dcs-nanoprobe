-- What each read of a sampling tier costs in the server state, per row, in
-- µs, on the objects that exist now, by class of object: units by category
-- and by whether they move, and statics. For each read: a Lua loop with the
-- function in a local, and the same calls driven from the DLL (`csweep`,
-- each call under lua_pcall). Also the parts of a full enumeration, and
-- whether a unit's methods take a plain table that holds only `id_`.
-- Each figure is the best of 7 passes less an empty loop's pass. One class
-- or part is measured a frame. Reads only; leaves nothing behind.
-- states: server
-- timeout: 600
local E = ...
local a = E.lib('agent').open()
local clock, csweep = a.clock, a.csweep
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

-- The objects by class.
local classes, order = {}, {}
local function add(name, o)
  local c = classes[name]
  if not c then
    c = { objs = {}, ids = {} }
    classes[name] = c
    order[#order + 1] = name
  end
  c.objs[#c.objs + 1] = o
  c.ids[#c.ids + 1] = o.id_
end
local KIND = { [0] = 'airplane', [1] = 'helicopter', [2] = 'ground', [3] = 'ship' }
local groups = {}
for side = 0, 2 do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    groups[#groups + 1] = g
    for _, u in ipairs(g:getUnits() or {}) do
      if u:isExist() then
        local d = u:getDesc()
        local kind = KIND[type(d) == 'table' and d.category or -1] or 'other'
        local v = u:getVelocity()
        local moving = v.x * v.x + v.y * v.y + v.z * v.z > 0.25
        add(kind .. (moving and '_moving' or '_halted'), u)
        add('units', u)
      end
    end
  end
  for _, s in ipairs(coalition.getStaticObjects(side) or {}) do
    if s:isExist() then
      add('statics', s)
    end
  end
end
table.sort(order)

-- At most `max` items of a list, evenly spread.
local function spread(list, max)
  if #list <= max then
    return list
  end
  local out = {}
  for i = 1, max do
    out[i] = list[math.floor((i - 1) * #list / max) + 1]
  end
  return out
end

-- One read over a list: the Lua loop, and the DLL's sweep.
local function read(list, f, nres)
  local n = #list
  if n == 0 or type(f) ~= 'function' then
    return nil
  end
  -- A read that raises on this class is reported, not timed.
  for i = 1, n do
    local ok, err = pcall(f, list[i])
    if not ok then
      return { rows = n, raised = tostring(err) }
    end
  end
  local empty = best(function()
    for i = 1, n do
      local _ = list[i]
    end
  end)
  local lua = best(function()
    for i = 1, n do
      f(list[i])
    end
  end)
  local c = best(function()
    csweep(f, list, nres)
  end)
  local bare = best(function()
    csweep(f, list, nres, 1)
  end)
  return { rows = n, lua = (lua - empty) / n, csweep = c / n, csweep_bare = bare / n }
end

local function measure(name)
  local c = classes[name]
  local objs, ids = c.objs, c.ids
  local first = objs[1]
  local unit = name ~= 'statics'
  local out = { rows = #objs }
  out.by_id = read(ids, DCS.getUnitPositionByObjectId, 3)
  local zero = 0
  for i = 1, #ids do
    local x = DCS.getUnitPositionByObjectId(ids[i])
    if x == 0 or x == nil then
      zero = zero + 1
    end
  end
  out.by_id_first_zero = zero
  out.is_exist = read(objs, first.isExist, 1)
  out.life = read(objs, first.getLife, 1)
  if unit then
    out.fuel = read(objs, first.getFuel, 1)
    local few = spread(objs, 300)
    out.ammo = read(few, first.getAmmo, 1)
    local none, entries = 0, 0
    for i = 1, #few do
      local t = few[i]:getAmmo()
      if type(t) == 'table' then
        entries = entries + #t
      else
        none = none + 1
      end
    end
    out.ammo_none, out.ammo_entries = none, entries
  end
  out.point = read(spread(objs, 500), first.getPoint, 1)
  out.position = read(spread(objs, 300), first.getPosition, 1)
  -- A plain table with the object's ID in place of the object.
  local proxy = { id_ = ids[1] }
  local ok, life = pcall(first.getLife, proxy)
  out.proxy_plain = ok and life == first:getLife()
  if not out.proxy_plain then
    setmetatable(proxy, getmetatable(first))
    ok, life = pcall(first.getLife, proxy)
    out.proxy_with_metatable = ok and life == first:getLife()
  end
  if out.proxy_plain or out.proxy_with_metatable then
    local f, n = first.getLife, #ids
    local empty = best(function()
      for i = 1, n do
        proxy.id_ = ids[i]
      end
    end)
    out.proxy_life = (best(function()
      for i = 1, n do
        proxy.id_ = ids[i]
        f(proxy)
      end
    end) - empty) / n
  end
  return out
end

-- The parts of a full enumeration.
local function enumeration()
  local out = { calls = {}, groups = #groups }
  local lists = 0
  for side = 0, 2 do
    for category = 0, 4 do
      local n = 0
      local t = best(function()
        n = #(coalition.getGroups(side, category) or {})
      end)
      if n > 0 then
        out.calls[#out.calls + 1] = { side = side, category = category, groups = n, us = t }
      end
      lists = lists + t
    end
    local n = 0
    local t = best(function()
      n = #(coalition.getStaticObjects(side) or {})
    end)
    if n > 0 then
      out.calls[#out.calls + 1] = { side = side, statics = n, us = t }
    end
    lists = lists + t
  end
  out.lists_us = lists
  local units = 0
  if #groups > 0 then
    local get = groups[1].getUnits
    out.get_units_us = best(function()
      units = 0
      for i = 1, #groups do
        units = units + #get(groups[i])
      end
    end)
    out.get_units_csweep_us = best(function()
      csweep(get, groups, 1, 1)
    end)
    local all = {}
    for i = 1, #groups do
      all[i] = get(groups[i])
    end
    local empty = best(function()
      for i = 1, #all do
        local l = all[i]
        for j = 1, #l do
          local _ = l[j]
        end
      end
    end)
    out.id_read_us = best(function()
      for i = 1, #all do
        local l = all[i]
        for j = 1, #l do
          local _ = rawget(l[j], 'id_')
        end
      end
    end) - empty
  end
  out.units = units
  return out
end

local function misc()
  local out = {}
  local N = 1000
  local empty = best(function()
    for _ = 1, N do
    end
  end)
  local function per(fn)
    return (best(fn) - empty) / N
  end
  out.collectgarbage_count = per(function()
    for _ = 1, N do
      collectgarbage('count')
    end
  end)
  out.timer_get_time = per(function()
    for _ = 1, N do
      timer.getTime()
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
  return out
end

-- One part a frame.
local result = { classes = {}, order = order }
local step = 0
return E.wait(function()
  step = step + 1
  local name = order[step]
  if name then
    result.classes[name] = measure(name)
    return nil
  end
  if step == #order + 1 then
    result.enumeration = enumeration()
    return nil
  end
  result.misc = misc()
  a.sink_take()
  return result
end, 540, 'running')
