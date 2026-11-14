-- What tells a static that can move (cargo) from one that cannot, and what
-- the static list costs as it grows. It adds four statics: a windsock, and
-- three of the cargo types of ED's Scripts/Database/db_countries.lua
-- (`default_Cargos`, ED-CODE). For each it reports the birth event, the
-- object's category, its description, the cargo getters, whether
-- coalition.getStaticObjects lists it, and what the position getter by
-- object ID returns for it. Then it adds windsocks up to 100, 300 and 1,000,
-- and times one coalition.getStaticObjects call at each count: alone in its
-- frame, and the best of five back to back. It destroys what it added.
-- states: server
-- timeout: 300
local E = ...
local S = E.lib('scenario')
local a = E.lib('agent').open()
local clock = a.clock

local function try(f, ...)
  if type(f) ~= 'function' then
    return { absent = true }
  end
  local r = { pcall(f, ...) }
  if not r[1] then
    return { raised = tostring(r[2]) }
  end
  local out = {}
  for i = 2, #r do
    local v = r[i]
    out[#out + 1] = (type(v) == 'table' or type(v) == 'userdata') and type(v) or v
  end
  out.n = #r - 1
  return out
end

local function keys(t)
  local out = {}
  if type(t) == 'table' then
    for k in pairs(t) do
      out[#out + 1] = tostring(k)
    end
    table.sort(out)
  end
  return out
end

local function field(name)
  local ok, v = pcall(function()
    local t = _G
    for part in name:gmatch('[^.]+') do
      t = t[part]
    end
    return t
  end)
  return ok and v or nil
end

-- Births seen while the probe runs, by object ID.
local births, events = {}, 0
local BIRTH = field('world.event.S_EVENT_BIRTH')
local handler = {}
function handler:onEvent(event)
  pcall(function()
    events = events + 1
    if rawget(event, 'id') ~= BIRTH then
      return
    end
    local o, d = rawget(event, 'initiator'), rawget(event, 'initiator_data')
    local id = type(o) == 'table' and rawget(o, 'id_')
    if id then
      births[id] = {
        object_category = type(d) == 'table' and rawget(d, 'object_category') or 'none',
        unit_type = type(d) == 'table' and rawget(d, 'unit_type') or 'none',
      }
    end
  end)
end
world.addEventHandler(handler)

local origin = S.origin()
local usa = country.id.USA
local side = coalition.side.BLUE
local mine = {}

local function add(name, data, north, east)
  data.name = name
  data.x, data.y, data.heading = origin.x + north, origin.z + east, 0
  local t0 = clock()
  local ok, o = pcall(coalition.addStaticObject, usa, data)
  mine[#mine + 1] = name
  local us = (clock() - t0) * 1e6
  if ok then
    return o, us, nil
  end
  return nil, us, tostring(o)
end

local CANDIDATES = {
  { 'np-design-windsock', { type = 'Windsock', category = 'Fortifications' } },
  { 'np-design-cargo-a', { type = 'ammo_cargo', category = 'Cargos', canCargo = true, mass = 1000 } },
  { 'np-design-cargo-b', { type = 'ammo_cargo', category = 'Cargos', canCargo = false } },
  { 'np-design-cargo-c', { type = 'container_cargo', category = 'Cargos', canCargo = true, mass = 1500 } },
}
local added = {}
for i, c in ipairs(CANDIDATES) do
  local _, us, err = add(c[1], c[2], -3000, i * 40)
  added[c[1]] = { add_us = us, add_error = err }
end

local function describe(name)
  local out = added[name]
  local o = StaticObject.getByName(name)
  if not o then
    out.found = false
    return out
  end
  local id = rawget(o, 'id_')
  out.found, out.id = true, id
  out.exists = try(o.isExist, o)
  out.object_category = try(field('Object.getCategory'), o)
  out.static_category = try(field('StaticObject.getCategory'), o)
  local ok, desc = pcall(o.getDesc, o)
  if ok and type(desc) == 'table' then
    out.desc_category = desc.category
    out.desc_keys = keys(desc)
    out.desc_attributes = keys(desc.attributes)
    out.desc_type = desc.typeName
  else
    out.desc_error = tostring(desc)
  end
  out.cargo_weight = try(field('StaticObject.getCargoWeight'), o)
  out.cargo_name = try(field('StaticObject.getCargoDisplayName'), o)
  out.has_attribute_cargos = try(o.hasAttribute, o, 'Cargos')
  out.type_name = try(o.getTypeName, o)
  out.life = try(o.getLife, o)
  out.velocity = try(field('Object.getVelocity'), o)
  local listed = false
  for _, s in ipairs(coalition.getStaticObjects(side) or {}) do
    if rawget(s, 'id_') == id then
      listed = true
    end
  end
  out.listed = listed
  out.by_id = try(field('DCS.getUnitPositionByObjectId'), id)
  local got, p = pcall(o.getPoint, o)
  out.point = got and type(p) == 'table' and { p.x, p.y, p.z } or tostring(p)
  out.birth = births[id] or 'none'
  return out
end

-- One coalition.getStaticObjects call, µs: alone in its frame, then the best
-- of five.
local function list_cost()
  local t0 = clock()
  local l = coalition.getStaticObjects(side)
  local first = (clock() - t0) * 1e6
  local best = math.huge
  for _ = 1, 5 do
    t0 = clock()
    coalition.getStaticObjects(side)
    best = math.min(best, (clock() - t0) * 1e6)
  end
  return { statics = type(l) == 'table' and #l or -1, alone_us = first, best_us = best }
end

local STEPS = { 100, 300, 1000 }
local PER_FRAME = 50
local start = timer.getTime()
local out = { categories = { object = field('Object.Category'), static = field('StaticObject.Category') }, scale = {} }
local filler, step, phase, settle = 0, 1, 'describe', 0
local add_us, add_n = 0, 0

return E.wait(function()
  if phase == 'describe' then
    -- Two seconds for the births to arrive.
    if timer.getTime() - start < 2 then
      return nil
    end
    out.candidates = {}
    for _, c in ipairs(CANDIDATES) do
      out.candidates[c[1]] = describe(c[1])
    end
    out.events_seen = events
    out.base = list_cost()
    phase = 'fill'
    return nil
  end
  if phase == 'fill' then
    local goal = STEPS[step]
    if not goal then
      phase = 'clear'
      return nil
    end
    if filler < goal then
      for _ = 1, math.min(PER_FRAME, goal - filler) do
        filler = filler + 1
        local _, us = add('np-design-fill-' .. filler, { type = 'Windsock', category = 'Fortifications' },
          -3200 - math.floor(filler / 50) * 30, (filler % 50) * 30)
        add_us, add_n = add_us + us, add_n + 1
      end
      settle = 0
      return nil
    end
    -- A few frames after the last add, so the call is alone in its frame.
    settle = settle + 1
    if settle < 5 then
      return nil
    end
    local cost = list_cost()
    cost.added = filler
    out.scale[#out.scale + 1] = cost
    step = step + 1
    return nil
  end
  -- Destroy what the probe added, a part each frame.
  local n = 0
  while #mine > 0 and n < 100 do
    local name = table.remove(mine)
    local o = StaticObject.getByName(name)
    if o then
      pcall(o.destroy, o)
    end
    n = n + 1
  end
  if #mine > 0 then
    return nil
  end
  world.removeEventHandler(handler)
  out.add_static_us = add_n > 0 and add_us / add_n or nil
  out.births_seen = #keys(births)
  return out
end, 240, 'running')
