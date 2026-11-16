-- Warehouse evidence: can a FARP added during the mission be made to offer
-- dynamic spawn? In order: (a) adds a second FARP, np-wh-farp2, with
-- `dynamicSpawn`, `allowHotStart` and `dynamicCargo` in its static table, and
-- stocks it; (b) reads `DCS.getDynamicSpawnSettings` for both FARPs under the
-- static ID, the airbase ID and the world ID, and whether
-- `DCS.getFarpsAndCarriersMissionData().farps` lists them; (c) sets those
-- fields in the mission's `env.warehouses.warehouses` entry of each FARP;
-- (d) reads as in (b) again.
-- Needs `farp-spawn.lua` run first (np-wh-farp). A mission load removes both.
-- Rule: recorded as read; a route works when any read gives
-- dynamicSpawnAvailable true for an added FARP. A person then checks the slot screen.
-- states: hook
-- timeout: 60

local E = ...
local function server(src)
  local ok, v = E.cross('server', src)
  if not ok then
    return 'cross failed: ' .. tostring(v)
  end
  return v
end

-- (a) A second FARP with the flags in its static table, stocked.
local added = server([[
local p = Airbase.getByName('Dwyer'):getPoint()
local ok, err = pcall(coalition.addStaticObject, country.id.USA, {
  category = 'Heliports', type = 'FARP', name = 'np-wh-farp2', x = p.x + 4000, y = p.z,
  heading = 0, heliport_callsign_id = 2, heliport_frequency = 128.5, heliport_modulation = 0,
  dynamicSpawn = true, allowHotStart = true, dynamicCargo = true,
})
local b = Airbase.getByName('np-wh-farp2')
if b then
  local wh = b:getWarehouse()
  pcall(wh.setItem, wh, 'UH-1H', 10)
  pcall(wh.setLiquidAmount, wh, 0, 50000)
end
return tostring(ok) .. ' ' .. tostring(err)
]])
E.observe('route4_add', added, { info = true })

local function ids(name)
  return server(([[
local s = StaticObject.getByName('%s')
local b = Airbase.getByName('%s')
local sid = s and tonumber(s:getID()) or -1
local bid = b and tonumber(b:getID()) or -1
local wid = (b and b.getWorldID) and tonumber(b:getWorldID()) or -1
local side = b and b:getCoalition() or -1
return sid .. ' ' .. bid .. ' ' .. wid .. ' ' .. side
]]):format(name, name))
end

local function read(label)
  local out = {}
  for _, name in ipairs({ 'np-wh-farp', 'np-wh-farp2' }) do
    local s = ids(name)
    local sid, bid, wid, side = tostring(s):match('(%-?%d+) (%-?%d+) (%-?%d+) (%-?%d+)')
    local entry = { static = sid, airbase = bid, world = wid, coalition = side }
    for _, k in ipairs({ tonumber(sid), tonumber(bid), tonumber(wid) }) do
      for _, air in ipairs({ false, true }) do
        local ok, v = pcall(DCS.getDynamicSpawnSettings, k, air)
        entry[tostring(k) .. '/' .. tostring(air)] = ok and type(v) == 'table'
          and (tostring(v.dynamicSpawnAvailable) .. ',' .. tostring(v.allowHotSpawn)) or tostring(v)
      end
    end
    out[name] = entry
  end
  local ok, f = pcall(DCS.getFarpsAndCarriersMissionData)
  local farps = {}
  if ok and type(f) == 'table' then
    for id, v in pairs(f.farps or {}) do
      farps[#farps + 1] = tostring(id) .. ' ' .. tostring(type(v) == 'table' and v.name or v)
    end
  else
    farps = tostring(f)
  end
  out.farps_listed = farps
  E.observe(label, out, { info = true })
end

-- (b) The observation names (route4_add, route1_ids, route2_env,
-- route2_after) are those of the live run.
read('route1_ids')

-- (c) The mission's warehouses table in the server state.
local r2 = server([[
local out = {}
local w = env.warehouses
if type(w) ~= 'table' then return 'env.warehouses is ' .. type(w) end
local keys = {}
for k in pairs(w.warehouses or {}) do keys[#keys + 1] = tostring(k) end
table.sort(keys)
out[#out + 1] = 'warehouses keys ' .. #keys .. ': ' .. table.concat(keys, ',', 1, math.min(#keys, 40))
local function find(name)
  local s = StaticObject.getByName(name)
  local b = Airbase.getByName(name)
  return (s and w.warehouses[tonumber(s:getID())]) or (b and w.warehouses[tonumber(b:getID())]), s, b
end
for _, name in ipairs({ 'np-wh-farp', 'np-wh-farp2' }) do
  local e, s, b = find(name)
  if not e and s then
    local ref = w.warehouses[8624]
    e = {}
    if type(ref) == 'table' then for k, v in pairs(ref) do e[k] = v end end
    e.coalition = 'blue'
    w.warehouses[tonumber(s:getID())] = e
    out[#out + 1] = name .. ': no entry; copied FARP London-1 entry under the static ID'
  end
  if e then
    e.dynamicSpawn = true
    e.allowHotStart = true
    out[#out + 1] = name .. ': flags set'
  end
end
return table.concat(out, ' | ')
]])
E.observe('route2_env', r2, { info = true })
read('route2_after')
return 'done'
