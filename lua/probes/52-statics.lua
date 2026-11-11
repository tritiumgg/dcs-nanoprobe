-- Static objects, airbases and a warehouse read.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')

local costs = {}
local first
for i = 1, 100 do
  local static, cost = S.spawn_static('np-static-' .. i, -600 - (i % 10) * 15, -600 - math.floor(i / 10) * 15)
  first = first or static
  costs[i] = cost
end
table.sort(costs)
-- coalition.addStaticObject: p50 123 us (LIVE on a client, another type).
E.observe('addStaticObject_us', costs[50] * 1e6, { windows = 123, timing = true })
E.observe('static_exists', first ~= nil and StaticObject.getByName('np-static-1') ~= nil, { windows = true })

-- DCS.getUnitPositionByObjectId is for units: a static's ID gives 0 (LIVE),
-- as the first of three values under Wine (LIVE).
local p = E.pack(DCS.getUnitPositionByObjectId(first.id_))
E.observe('static_position_first', p[1], { windows = 0 })
E.observe('static_position_values', p.n, { wine = 3 })

local bases = world.getAirbases()
E.observe('airbases', #bases, { info = true })
-- An airbase warehouse's inventory holds four liquids, keyed 0 to 3 (LIVE).
local inventory = bases[1]:getWarehouse():getInventory()
local liquids = {}
for key in pairs(inventory.liquids or {}) do
  liquids[#liquids + 1] = key
end
table.sort(liquids)
E.observe('warehouse_liquids', liquids, { windows = { 0, 1, 2, 3 } })
local keys = {}
for key in pairs(inventory) do
  keys[#keys + 1] = tostring(key)
end
table.sort(keys)
E.observe('warehouse_keys', keys, { info = true })

S.clear()
return 'done'
