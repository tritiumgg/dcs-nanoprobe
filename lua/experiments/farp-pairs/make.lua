-- FARP pairs, part 3: can a script make a static group of two FARPs at run time?
-- One try per call, chosen by `<try>` (run.sh fills it in), each 400 m or more east
-- of the mission's pair `np-hg-1`:
--   group-static  coalition.addGroup(USA, -1, group of two Heliports units)
--   static-units  coalition.addStaticObject(USA, a FARP whose table also lists the
--                 two units under `units`)
--   same-group    two coalition.addStaticObject(USA, FARP) calls with one `groupId`
--                 and their own `unitId`s
--   group-ground  coalition.addGroup(USA, Group.Category.GROUND, group of two
--                 Heliports units)
--   group-dynamic as group-static, with `dynamicSpawn = true` on each unit and on
--                 the group (ADR 0032: a FARP static reads the key at spawn)
-- After the try, read every name it used as an airbase, a static and a group, the
-- airbase count, and, when two pads exist, whether their warehouses share stock
-- (jet fuel set at the first, read at both).
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local TRY = '<try>'
local TRIES = { ['group-static'] = 400, ['static-units'] = 800, ['same-group'] = 1200, ['group-ground'] = 1600, ['group-dynamic'] = 2000 }
local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then return v end
  return 'raised: ' .. tostring(v)
end
-- Call method `m` of `obj`; the lookup is inside the pcall too, so a missing
-- method reads as raised.
local function call(obj, m, ...)
  local args = { ... }
  local n = select('#', ...)
  return try(function() return obj[m](obj, unpack(args, 1, n)) end)
end
local function get(f)
  return try(f)
end
local dy = TRIES[TRY]
if not dy then error('unknown try ' .. TRY) end
local anchor = Airbase.getByName('np-hg-1')
if not anchor then error('no np-hg-1: load the heliport-group mission first') end
local p = anchor:getPoint()
local tag = 'np-' .. TRY
local function unit(k, id)
  return { category = 'Heliports', type = 'FARP', shape_name = 'FARPS', name = tag .. '-' .. k,
    unitId = id, x = p.x + 200 * (k - 1), y = p.z + dy, heading = 0, rate = 100,
    heliport_frequency = '127.5', heliport_modulation = 0, heliport_callsign_id = 1 }
end
local before = #world.getAirbases()
local out = { try = TRY, airbases_before = before, surface = {} }
for k = 1, 2 do
  local u = unit(k)
  out.surface[k] = get(function() return land.getSurfaceType({ x = u.x, y = u.y }) end)
end

local made
if TRY == 'group-static' or TRY == 'group-ground' or TRY == 'group-dynamic' then
  local cat = TRY == 'group-ground' and Group.Category.GROUND or -1
  local g = { name = tag, task = 'Ground Nothing', x = p.x, y = p.z + dy, units = { unit(1), unit(2) } }
  if TRY == 'group-dynamic' then
    g.dynamicSpawn = true
    for _, u in ipairs(g.units) do u.dynamicSpawn = true end
  end
  made = try(coalition.addGroup, country.id.USA, cat, g)
elseif TRY == 'static-units' then
  local s = unit(1)
  s.units = { unit(1), unit(2) }
  made = try(coalition.addStaticObject, country.id.USA, s)
else
  local a, b = unit(1, 900101), unit(2, 900102)
  a.groupId, b.groupId = 900100, 900100
  a.groupName, b.groupName = tag, tag
  made = { try(coalition.addStaticObject, country.id.USA, a), try(coalition.addStaticObject, country.id.USA, b) }
end
local function describe(v)
  if type(v) ~= 'table' then return tostring(v) end
  local class = get(function() return v.className_ end)
  if type(class) == 'string' and not class:find('^raised') then return { class = class, id = rawget(v, 'id_') } end
  local r = {}
  for k, x in pairs(v) do r[tostring(k)] = describe(x) end
  return r
end
out.returned = describe(made)
out.airbases_after = #world.getAirbases()
out.names = {}
local whs = {}
for _, n in ipairs({ tag, tag .. '-1', tag .. '-2' }) do
  local b = try(Airbase.getByName, n)
  local s = try(StaticObject.getByName, n)
  local g = try(Group.getByName, n)
  local r = {
    airbase = type(b) == 'table' and { id = rawget(b, 'id_'), category = call(b, 'getCategory'),
      type = call(b, 'getTypeName') } or tostring(b),
    static = type(s) == 'table' and { id = rawget(s, 'id_'), class = get(function() return s.className_ end) } or tostring(s),
    group = type(g) == 'table' and { id = rawget(g, 'id_'), size = call(g, 'getSize') } or tostring(g),
  }
  if type(b) == 'table' then
    local wh = call(b, 'getWarehouse')
    if type(wh) == 'table' then
      whs[#whs + 1] = wh
      r.warehouse = { id = rawget(wh, 'id_'), jet_fuel = call(wh, 'getLiquidAmount', 0) }
    end
  end
  out.names[n] = r
end
if #whs >= 2 then
  call(whs[1], 'setLiquidAmount', 0, 1234)
  out.shared = { first = call(whs[1], 'getLiquidAmount', 0), second = call(whs[2], 'getLiquidAmount', 0) }
end
return out
