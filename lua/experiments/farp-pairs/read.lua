-- FARP pairs, part 1: in a mission made with `cargo xtask miz ... --heliport-group
-- X,Y,2,first|each` (one static group `np-hg` of two FARPs, `np-hg-1` and `np-hg-2`),
-- read what DCS made of the pair: airbases by each unit name and by the group name,
-- the `world.getAirbases` entries, each pad's warehouse and stock, the mission's
-- `warehouses` table as DCS loaded it, and the surface under each unit. Then set
-- Mk-82 at the first pad's warehouse and jet fuel at the second's, and read both, to
-- see whether the two pads share one stock. The sets are put back after the reads.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local NAMES = { 'np-hg-1', 'np-hg-2', 'np-hg' }
local MK82 = 'weapons.bombs.Mk_82'

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

local function stock(wh)
  if type(wh) ~= 'table' then return { warehouse = tostring(wh) } end
  return {
    id = rawget(wh, 'id_'),
    jet_fuel = call(wh, 'getLiquidAmount', 0),
    mk82 = call(wh, 'getItemCount', MK82),
  }
end

local function base(name)
  local b = try(Airbase.getByName, name)
  if type(b) ~= 'table' then return { airbase = tostring(b) } end
  local r = { id = rawget(b, 'id_'), get_id = call(b, 'getID'), name = call(b, 'getName'),
    category = call(b, 'getCategory'), type = call(b, 'getTypeName'),
    coalition = call(b, 'getCoalition') }
  local d = call(b, 'getDesc')
  r.desc_category = type(d) == 'table' and d.category or d
  local p = call(b, 'getPoint')
  if type(p) == 'table' then
    r.x, r.z = p.x, p.z
    r.surface = get(function() return land.getSurfaceType({ x = p.x, y = p.z }) end)
  end
  r.parking = (function() local pk = call(b, 'getParking') return type(pk) == 'table' and #pk or pk end)()
  r.unit1 = (function() local u = call(b, 'getUnit', 1) return type(u) == 'table' and call(u, 'getName') or tostring(u) end)()
  local wh = call(b, 'getWarehouse')
  r.stock = stock(wh)
  return r, wh
end

local out = { airbases = {}, bases = {}, statics = {}, mission_warehouses = {} }
for _, b in ipairs(world.getAirbases()) do
  local d = call(b, 'getDesc')
  if type(d) == 'table' and d.category ~= 0 then
    out.airbases[#out.airbases + 1] = { name = call(b, 'getName'), id = rawget(b, 'id_'),
      category = d.category, exists = call(b, 'isExist') }
  end
end
out.airbase_count = #world.getAirbases()
local whs = {}
for _, n in ipairs(NAMES) do
  out.bases[n], whs[n] = base(n)
  local s = try(StaticObject.getByName, n)
  out.statics[n] = type(s) == 'table' and { id = rawget(s, 'id_'), class = get(function() return s.className_ end) } or tostring(s)
end
local listed = {}
for _, side in ipairs({ 0, 1, 2 }) do
  for _, s in ipairs(coalition.getStaticObjects(side)) do
    listed[#listed + 1] = call(s, 'getName')
  end
end
out.get_static_objects = listed
local mw = get(function() return env.warehouses.warehouses end)
if type(mw) == 'table' then
  for id, e in pairs(mw) do
    out.mission_warehouses[tostring(id)] = { jet_fuel = e.jet_fuel and e.jet_fuel.InitFuel,
      unlimitedFuel = e.unlimitedFuel, dynamicSpawn = e.dynamicSpawn }
  end
else
  out.mission_warehouses = tostring(mw)
end

-- Shared stock: change one pad's warehouse and read both.
local w1, w2 = whs['np-hg-1'], whs['np-hg-2']
if type(w1) == 'table' and type(w2) == 'table' then
  local before = { one = stock(w1), two = stock(w2) }
  call(w1, 'setItem', MK82, 7)
  local after_mk82 = { one = stock(w1), two = stock(w2) }
  call(w2, 'setLiquidAmount', 0, 4321)
  local after_fuel = { one = stock(w1), two = stock(w2) }
  out.shared = { before = before, after_mk82_set_7_at_one = after_mk82,
    after_jet_fuel_set_4321_at_two = after_fuel }
  -- Put back what the reads found first.
  if type(before.one.mk82) == 'number' then call(w1, 'setItem', MK82, before.one.mk82) end
  if type(before.two.jet_fuel) == 'number' then call(w2, 'setLiquidAmount', 0, before.two.jet_fuel) end
  if type(before.one.jet_fuel) == 'number' then call(w1, 'setLiquidAmount', 0, before.one.jet_fuel) end
  out.restored = { one = stock(w1), two = stock(w2) }
end
return out
