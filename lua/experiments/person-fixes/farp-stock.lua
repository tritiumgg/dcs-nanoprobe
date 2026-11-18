-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): A spawned FARP's coalition, aircraft and jet fuel. The FARP name is the run's.
-- states: server
local b = Airbase.getByName('np#4XF1')
local i = b:getWarehouse():getInventory()
local a = {}
for k, v in pairs(i.aircraft or {}) do a[#a + 1] = k .. '=' .. v end
return { coalition = b:getCoalition(), aircraft = table.concat(a, ' '), jet = (i.liquids or {})[0] }
