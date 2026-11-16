-- Warehouse evidence: spawn a FARP during the mission, 2 km north of the
-- first airdrome whose inventory lists an aircraft, and read whether it has a warehouse, whether that
-- warehouse is found as an airbase, and what its inventory holds. Then set
-- one Mk-82 count and read it back, to see whether it is limited. A mission
-- load removes the FARP.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local E = ...
local anchor
for _, base in ipairs(world.getAirbases()) do
  local ok, d = pcall(base.getDesc, base)
  if not anchor and ok and d.category == 0 then
    local inv = base:getWarehouse():getInventory()
    if next(inv.aircraft or {}) then
      anchor = base
    end
  end
end
if not anchor then
  error('no airdrome with a listed aircraft inventory')
end
local p = anchor:getPoint()
local name = 'np-wh-farp'
local ok, err = pcall(coalition.addStaticObject, country.id.USA, {
  category = 'Heliports', type = 'FARP', name = name, x = p.x + 2000, y = p.z,
  heading = 0, heliport_callsign_id = 1, heliport_frequency = 127.5, heliport_modulation = 0,
})
E.observe('spawn', { anchor = anchor:getName(), ok = ok, error = (not ok) and tostring(err) or nil }, { info = true })

local function count(t)
  local n = 0
  for _ in pairs(t or {}) do
    n = n + 1
  end
  return n
end

local base = Airbase.getByName(name)
E.observe('as_airbase', base ~= nil, { info = true })
if not base then
  return 'no airbase'
end
local okw, wh = pcall(base.getWarehouse, base)
E.observe('has_warehouse', okw and wh ~= nil, { info = true })
if not (okw and wh) then
  return 'no warehouse'
end
local inv = wh:getInventory()
local liquids = {}
for k, v in pairs(inv.liquids or {}) do
  liquids[#liquids + 1] = tostring(k) .. '=' .. tostring(v)
end
table.sort(liquids)
E.observe('inventory', { aircraft = count(inv.aircraft), weapon = count(inv.weapon), liquids = liquids,
  mk82 = wh:getItemCount('weapons.bombs.Mk_82') }, { info = true })
pcall(wh.setItem, wh, 'weapons.bombs.Mk_82', 7)
pcall(wh.setLiquidAmount, wh, 0, 1234)
E.observe('set_read_back', { mk82 = wh:getItemCount('weapons.bombs.Mk_82'), jet_fuel = wh:getLiquidAmount(0) },
  { info = true })
return 'done'
