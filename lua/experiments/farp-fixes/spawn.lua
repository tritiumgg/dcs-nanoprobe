-- FARP fixes, part 1 of 3: add three heliport statics by script, 2 km and more
-- north of the first airdrome, and read how DCS exposes each: the birth event,
-- the static's class and reads, whether `coalition.getStaticObjects` lists it,
-- and the airbase it makes, with its callsign and radio. `destroy.lua` and
-- `after.lua` follow, in later calls. The objects are kept in the global
-- `np_farp_fixes` between the calls; `after.lua` removes them.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local SPECS = {
  -- A FARP with its radio as numbers.
  { name = 'np-ff-farp', type = 'FARP', shape_name = 'FARPS', dx = 2000,
    heliport_callsign_id = 3, heliport_frequency = 129.5, heliport_modulation = 0 },
  -- A helipad with its radio as a saved file holds it: the frequency as text.
  { name = 'np-ff-pad', type = 'SINGLE_HELIPAD', shape_name = 'FARP', dx = 2300,
    heliport_callsign_id = 2, heliport_frequency = '127.5', heliport_modulation = 1 },
  -- A helipad with no radio keys.
  { name = 'np-ff-bare', type = 'SINGLE_HELIPAD', shape_name = 'FARP', dx = 2600 },
  -- A FARP with the mission file's warehouse flags in its unit table: does
  -- `addStaticObject` read them and make its warehouse unlimited?
  { name = 'np-ff-unl', type = 'FARP', shape_name = 'FARPS', dx = 2900,
    unlimitedFuel = true, unlimitedMunitions = true, unlimitedAircrafts = true },
}

-- A warehouse's reads: an unlimited one reads 1,000,000 from the counts
-- (LIVE, ADR 0032).
local function stock(ab)
  local ok, wh = pcall(ab.getWarehouse, ab)
  if not (ok and wh) then
    return { warehouse = ok and 'none' or ('raised: ' .. tostring(wh)) }
  end
  local r = { id = rawget(wh, 'id_') }
  r.jet_fuel = select(2, pcall(wh.getLiquidAmount, wh, 0))
  r.mk82 = select(2, pcall(wh.getItemCount, wh, 'weapons.bombs.Mk_82'))
  r.uh1 = select(2, pcall(wh.getItemCount, wh, 'UH-1H'))
  local okr, map = pcall(function() return wh:getResourceMap() end)
  r.resource_map = okr and type(map) or ('raised: ' .. tostring(map))
  return r, wh
end

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

local births = {}
local handler = {}
function handler:onEvent(e)
  local ok, id = pcall(function()
    return rawget(e.initiator, 'id_')
  end)
  births[#births + 1] = { event = e.id, id = ok and id or nil,
    category = ok and try(Object.getCategory, e.initiator) or nil }
end
world.addEventHandler(handler)

local G = { names = {}, statics = {}, bases = {} }
_G.np_farp_fixes = G
local out = { anchor = anchor:getName(), airbases_before = #world.getAirbases(), objects = {} }
for _, spec in ipairs(SPECS) do
  local s = {
    category = 'Heliports', type = spec.type, shape_name = spec.shape_name, name = spec.name,
    x = p.x + spec.dx, y = p.z, heading = 0,
    heliport_callsign_id = spec.heliport_callsign_id,
    heliport_frequency = spec.heliport_frequency,
    heliport_modulation = spec.heliport_modulation,
    unlimitedFuel = spec.unlimitedFuel, unlimitedMunitions = spec.unlimitedMunitions,
    unlimitedAircrafts = spec.unlimitedAircrafts,
  }
  local ok, made = pcall(coalition.addStaticObject, country.id.USA, s)
  local r = { added = ok and made ~= nil, error = (not ok) and tostring(made) or nil }
  G.names[#G.names + 1] = spec.name
  if ok and made then
    r.returned_id = rawget(made, 'id_')
    r.returned_class = rawget(made, 'className_') or try(function() return made.className_ end)
  end
  local st = StaticObject.getByName(spec.name)
  if st then
    G.statics[spec.name] = st
    r.static = {
      id = rawget(st, 'id_'), class = try(function() return st.className_ end),
      exists = try(st.isExist, st), object_category = try(Object.getCategory, st),
      desc_category = try(function() return st:getDesc().category end),
      life = try(st.getLife, st), coalition = try(st.getCoalition, st),
      country = try(st.getCountry, st), type = try(st.getTypeName, st),
      point = try(function() local q = st:getPoint() return { q.x, q.y, q.z } end),
      life_by_id = try(StaticObject.getLife, { id_ = rawget(st, 'id_') }),
      exists_by_id = try(Object.isExist, { id_ = rawget(st, 'id_') }),
    }
  end
  local listed = {}
  for side = 0, 2 do
    for _, o in ipairs(coalition.getStaticObjects(side) or {}) do
      if try(o.getName, o) == spec.name then
        listed[#listed + 1] = side
      end
    end
  end
  r.in_get_static_objects = listed
  local ab = Airbase.getByName(spec.name)
  if ab then
    G.bases[spec.name] = ab
    local comm = try(ab.getCommunicator, ab)
    local comm_keys
    if type(comm) == 'table' then
      comm_keys = {}
      local mt = getmetatable(comm)
      local idx = mt and mt.__index
      for k in pairs(type(idx) == 'table' and idx or {}) do
        comm_keys[#comm_keys + 1] = tostring(k)
      end
      for k in pairs(comm) do
        comm_keys[#comm_keys + 1] = 'own:' .. tostring(k)
      end
      table.sort(comm_keys)
    end
    r.airbase = {
      id = rawget(ab, 'id_'), category = try(ab.getCategory, ab),
      desc_category = try(function() return ab:getDesc().category end),
      callsign = try(ab.getCallsign, ab), silent = try(ab.getRadioSilentMode, ab),
      communicator = type(comm) == 'table' and 'table' or tostring(comm), communicator_keys = comm_keys,
      frequency = type(comm) == 'table' and try(function() return comm:getFrequency() end) or nil,
      modulation = type(comm) == 'table' and try(function() return comm:getModulation() end) or nil,
      world_id = try(ab.getWorldID, ab),
    }
    r.stock = stock(ab)
  end
  out.objects[spec.name] = r
end
world.removeEventHandler(handler)
-- Whether two FARPs share stock: set jet fuel at the FARP, read it at the
-- radio helipad.
local farp, pad = G.bases['np-ff-farp'], G.bases['np-ff-pad']
if farp and pad then
  local _, wa = stock(farp)
  local _, wb = stock(pad)
  if wa and wb then
    local before = select(2, pcall(wb.getLiquidAmount, wb, 0))
    pcall(wa.setLiquidAmount, wa, 0, 4321)
    out.shared = { farp = select(2, pcall(wa.getLiquidAmount, wa, 0)), pad_before = before,
      pad_after = select(2, pcall(wb.getLiquidAmount, wb, 0)) }
  end
end
out.events = births
out.airbases_after = #world.getAirbases()
return out
