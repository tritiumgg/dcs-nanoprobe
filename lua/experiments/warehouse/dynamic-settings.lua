-- Warehouse evidence: what `DCS.getDynamicSpawnSettings(id, isAirdrome)` returns
-- for the airdrome Dwyer (11), the mission's FARP (8624) and the FARP added
-- during the mission (`np-wh-farp`, its ID read in the server state). ED's
-- dynamic slot dialog reads `dynamicSpawnAvailable` off the result (ED-CODE,
-- Scripts/UI/MultiplayerSelectRoleMap/MultiplayerSelectDynamicDialog.lua,
-- around line 872, on DCS 2.9.31.29434).
-- Rule: recorded as read.
-- states: hook
-- timeout: 30

local E = ...
local function dump(v)
  if type(v) ~= 'table' then
    return tostring(v)
  end
  local out = {}
  for k, x in pairs(v) do
    out[tostring(k)] = tostring(x)
  end
  return out
end
local ok, added = E.cross('server', "local b = Airbase.getByName('np-wh-farp') return b and b:getID() or -1")
local ids = { { 'Dwyer', 11, true }, { 'FARP London-1', 8624, false } }
if ok and tonumber(added) and tonumber(added) >= 0 then
  ids[#ids + 1] = { 'np-wh-farp', tonumber(added), false }
end
local out = {}
for _, e in ipairs(ids) do
  local okc, v = pcall(DCS.getDynamicSpawnSettings, e[2], e[3])
  out[e[1]] = { id = e[2], ok = okc, value = okc and dump(v) or tostring(v) }
end
E.observe('settings', out, { info = true })
return 'done'
