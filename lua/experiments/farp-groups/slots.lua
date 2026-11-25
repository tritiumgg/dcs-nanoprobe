-- FARP groups: the hook's view of the airbases `spawn.lua` made: each entry of
-- `DCS.getFarpsAndCarriersMissionData().farps` whose name starts `np-fg-`,
-- with `DCS.getDynamicSpawnSettings(key, false)`. Run after `spawn.lua`.
-- Rule: recorded as read.
-- states: hook
-- timeout: 30

local data = DCS.getFarpsAndCarriersMissionData()
local out = {}
for key, f in pairs(type(data) == 'table' and data.farps or {}) do
  local name = type(f) == 'table' and tostring(f.name) or ''
  if name:sub(1, 6) == 'np-fg-' then
    local ok, s = pcall(DCS.getDynamicSpawnSettings, key, false)
    local r = { key = key, type = f.type }
    if ok and type(s) == 'table' then
      r.dynamic, r.hot = s.dynamicSpawnAvailable, s.allowHotSpawn
    else
      r.dynamic = ok and tostring(s) or ('raised: ' .. tostring(s))
    end
    out[name] = r
  end
end
return out
