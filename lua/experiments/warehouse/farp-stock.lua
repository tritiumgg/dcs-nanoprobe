-- Warehouse evidence: stock the FARP `farp-spawn.lua` added (`np-wh-farp`) so a
-- player could spawn there: UH-1H and Mi-8MT airframes and jet fuel. Reads the
-- counts back. Whether DCS offers a dynamic spawn there is read separately
-- (`dynamic-settings.lua`) and by a person at a client.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local E = ...
local base = Airbase.getByName('np-wh-farp')
if not base then
  error('np-wh-farp not found; run farp-spawn.lua first')
end
local wh = base:getWarehouse()
local set = {}
for _, t in ipairs({ 'UH-1H', 'Mi-8MT' }) do
  local ok, err = pcall(wh.setItem, wh, t, 10)
  set[t] = { ok = ok, error = (not ok) and tostring(err) or nil, count = wh:getItemCount(t) }
end
local okf = pcall(wh.setLiquidAmount, wh, 0, 50000)
E.observe('stock', { id = base:getID(), set = set, fuel_set = okf, jet_fuel = wh:getLiquidAmount(0) }, { info = true })
return 'done'
