-- FARP groups, part 1: on a paused mission, add static groups with
-- `coalition.addGroup(country, -1, group)` east of the first airdrome, and read
-- what DCS makes of each. The groups:
--   g3   3 FARPs, a `unitId` on unit 1, `dynamicSpawn` on the group only
--   g2u  2 FARPs, `dynamicSpawn` on unit 1 only
--   g10  10 FARPs
--   h2, h3  2 and 3 SINGLE_HELIPADs
--   fh   a FARP, then a SINGLE_HELIPAD
--   f2   2 fortifications
--   ff   a FARP, then a fortification
--   ff2  2 FARPs, then 2 fortifications
--   fv   a FARP, then a ground vehicle (M-113)
--   p3, p2, p1, r3, r2, r1   the starting points of the replace tries
--        (`replace.lua` takes the p ones while paused, `run.lua` the r ones
--        while running): 3 FARPs, 2 FARPs, one FARP by addStaticObject
--   d2   2 FARPs whose second unit `run.lua` hits with an explosion
-- Each read is kept in the global `np_farp_groups` with a reader that later
-- parts call; an event handler records every event whose initiator or target
-- is an object these groups made. `cleanup.lua` removes the handler.
-- Rule: recorded as read.
-- states: server
-- timeout: 60

local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then
    return v
  end
  return 'raised: ' .. tostring(v)
end

local anchor
for _, base in ipairs(world.getAirbases()) do
  local ok, d = pcall(base.getDesc, base)
  if not anchor and ok and type(d) == 'table' and d.category == 0 then
    anchor = base
  end
end
if not anchor then
  error('no airdrome')
end
local p = anchor:getPoint()

-- Groups in rows 600 m apart, 3 km east of the airdrome; units 200 m apart
-- to the north, as in the farp-pairs run.
local TYPES = {
  FARP = { category = 'Heliports', type = 'FARP', shape_name = 'FARPS' },
  PAD = { category = 'Heliports', type = 'SINGLE_HELIPAD', shape_name = 'FARP' },
  FORT = { category = 'Fortifications', type = 'Hangar A' },
  VEH = { type = 'M-113' },
}
local GROUPS = {
  { tag = 'g3', units = { 'FARP', 'FARP', 'FARP' }, unit_id = 900201, group_dynamic = true },
  { tag = 'g2u', units = { 'FARP', 'FARP' }, unit1_dynamic = true },
  { tag = 'g10', units = { 'FARP', 'FARP', 'FARP', 'FARP', 'FARP', 'FARP', 'FARP', 'FARP', 'FARP', 'FARP' } },
  { tag = 'h2', units = { 'PAD', 'PAD' } },
  { tag = 'h3', units = { 'PAD', 'PAD', 'PAD' } },
  { tag = 'fh', units = { 'FARP', 'PAD' } },
  { tag = 'f2', units = { 'FORT', 'FORT' } },
  { tag = 'ff', units = { 'FARP', 'FORT' } },
  { tag = 'ff2', units = { 'FARP', 'FARP', 'FORT', 'FORT' } },
  { tag = 'fv', units = { 'FARP', 'VEH' } },
  { tag = 'p3', units = { 'FARP', 'FARP', 'FARP' } },
  { tag = 'p2', units = { 'FARP', 'FARP' } },
  { tag = 'p1', units = { 'FARP' }, single = true },
  { tag = 'r3', units = { 'FARP', 'FARP', 'FARP' } },
  { tag = 'r2', units = { 'FARP', 'FARP' } },
  { tag = 'r1', units = { 'FARP' }, single = true },
  { tag = 'd2', units = { 'FARP', 'FARP' } },
}

local G = { groups = {}, ids = {}, events = {}, places = {} }
_G.np_farp_groups = G
local x0, z0 = p.x, p.z + 3000

-- The unit table of unit k of a group whose row starts at `row`.
function G.unit(tag, kind, k, row)
  local t = TYPES[kind]
  local u = { category = t.category, type = t.type, shape_name = t.shape_name,
    name = 'np-fg-' .. tag .. '-' .. k, x = x0 + 200 * (k - 1), y = z0 + 600 * row, heading = 0, rate = 100 }
  G.places[u.name] = { x = u.x, z = u.y }
  return u
end

-- What DCS returned: its class, ID and category.
function G.describe(v)
  if type(v) ~= 'table' then
    return tostring(v)
  end
  return { class = try(function() return v.className_ end), id = rawget(v, 'id_'),
    category = try(Object.getCategory, v), name = try(function() return v:getName() end) }
end

-- A warehouse's whole stock, summarized: item and liquid counts.
local function stock(ab)
  local wh = try(ab.getWarehouse, ab)
  if type(wh) ~= 'table' then
    return tostring(wh)
  end
  local inv = try(wh.getInventory, wh)
  if type(inv) ~= 'table' then
    return { id = rawget(wh, 'id_'), inventory = tostring(inv) }
  end
  local r = { id = rawget(wh, 'id_'), aircraft = 0, weapon = 0, liquids = {} }
  for _, n in pairs(inv.aircraft or {}) do
    r.aircraft = r.aircraft + (tonumber(n) or 0)
  end
  for _, n in pairs(inv.weapon or {}) do
    r.weapon = r.weapon + (tonumber(n) or 0)
  end
  for k, n in pairs(inv.liquids or {}) do
    r.liquids[tostring(k)] = n
  end
  return r
end

-- Every read of a group by its unit names: what each name finds as an
-- airbase, a static and a group, the airbase's parking and stock, and what
-- stands at each unit's place.
function G.read(tag, n)
  local r = { names = {} }
  for k = 1, n do
    local name = 'np-fg-' .. tag .. '-' .. k
    local ab, st = try(Airbase.getByName, name), try(StaticObject.getByName, name)
    local un = try(Unit.getByName, name)
    local e = {
      unit = type(un) == 'table' and { id = rawget(un, 'id_'), exists = try(un.isExist, un) } or tostring(un),
      airbase = type(ab) == 'table' and rawget(ab, 'id_') or tostring(ab),
      static = type(st) == 'table' and { id = rawget(st, 'id_'), exists = try(st.isExist, st),
        class = try(function() return st.className_ end) } or tostring(st),
    }
    if k == 1 and type(ab) == 'table' then
      r.get_id = try(ab.getID, ab)
      r.exists = try(ab.isExist, ab)
      r.life = try(ab.getLife, ab)
      local parking = try(ab.getParking, ab)
      r.parking = type(parking) == 'table' and #parking or parking
      r.stock = stock(ab)
    end
    local at = G.places[name]
    if at then
      local found = {}
      local vol = { id = world.VolumeType.SPHERE, params = { point = { x = at.x, y = land.getHeight({ x = at.x, y = at.z }), z = at.z }, radius = 40 } }
      try(world.searchObjects, { Object.Category.UNIT, Object.Category.STATIC, Object.Category.BASE, Object.Category.SCENERY }, vol,
        function(o)
          found[#found + 1] = { id = rawget(o, 'id_'), category = try(Object.getCategory, o), type = try(o.getTypeName, o) }
          return true
        end)
      e.near = found
      e.surface = try(land.getSurfaceType, { x = at.x, y = at.z })
    end
    r.names[name] = e
  end
  r.group = tostring(try(Group.getByName, 'np-fg-' .. tag))
  return r
end

-- Spawn one group as a static group, or unit 1 by addStaticObject.
function G.spawn(spec, row)
  local units = {}
  for k, kind in ipairs(spec.units) do
    units[k] = G.unit(spec.tag, kind, k, row)
  end
  if spec.unit_id then
    units[1].unitId = spec.unit_id
  end
  if spec.unit1_dynamic then
    units[1].dynamicSpawn = true
  end
  local made
  if spec.single then
    made = try(coalition.addStaticObject, country.id.USA, units[1])
  else
    local g = { name = 'np-fg-' .. spec.tag, units = units, dynamicSpawn = spec.group_dynamic or nil }
    made = try(coalition.addGroup, country.id.USA, -1, g)
  end
  if type(made) == 'table' then
    G.ids[rawget(made, 'id_') or -1] = spec.tag
  end
  return made
end

-- The replace tries on the group `tag` (p3, p2, p1, r3, r2 or r1): 3 FARPs
-- become 2, 2 become one by addStaticObject, one becomes 2. Reads the old
-- object by name before, and every name after.
local NEXT = { ['3'] = { 'FARP', 'FARP' }, ['2'] = { 'FARP' }, ['1'] = { 'FARP', 'FARP' } }
function G.replace(tag)
  local spec = G.groups[tag]
  local row
  for i, t in ipairs(G.order) do
    if t == tag then
      row = i
    end
  end
  local old = try(StaticObject.getByName, 'np-fg-' .. tag .. '-1')
  G.old = G.old or {}
  G.old[tag] = old
  local units = NEXT[tag:sub(2)]
  local made = G.spawn({ tag = tag, units = units, single = #units == 1 }, row)
  local r = G.read(tag, 3)
  r.old = G.describe(old)
  r.old_exists = type(old) == 'table' and try(old.isExist, old) or nil
  r.returned = G.describe(made)
  spec.now = #units
  return r
end

local handler = {}
function handler:onEvent(e)
  local function id_of(o)
    local ok, id = pcall(function() return rawget(o, 'id_') end)
    return ok and id or nil
  end
  local i, t = id_of(e.initiator), id_of(e.target)
  if G.ids[i] or G.ids[t] then
    G.events[#G.events + 1] = { event = e.id, initiator = i, target = t, model = timer.getTime() }
  end
end
G.handler = handler
world.addEventHandler(handler)

local out = { anchor = anchor:getName(), airbases_before = #world.getAirbases(), groups = {} }
G.order = {}
for row, spec in ipairs(GROUPS) do
  G.groups[spec.tag] = spec
  G.order[row] = spec.tag
  local before = #world.getAirbases()
  local made = G.spawn(spec, row)
  local r = G.read(spec.tag, #spec.units)
  r.returned = G.describe(made)
  r.new_airbases = #world.getAirbases() - before
  out.groups[spec.tag] = r
end
out.airbases_after = #world.getAirbases()
out.events = G.events
return out
