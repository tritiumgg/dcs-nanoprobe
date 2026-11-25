-- FARP pairs, part 2: in the hook state, what the slot screen's dynamic-spawn list
-- reads for the pair: `DCS.getFarpsAndCarriersMissionData().farps`, and
-- `DCS.getDynamicSpawnSettings(id, false)` for each listed FARP and for each pad's
-- airbase ID, which ED's dialog checks before it lists a FARP (ED-CODE,
-- Scripts/UI/MultiplayerSelectRoleMap/MultiplayerSelectDynamicDialog.lua,
-- getDynamicsFarpsAndCarriers). Also `DCS.getAvailableSlots('blue')`'s count.
-- Rule: recorded as read.
-- states: hook
-- timeout: 30

local E = ...
local function flat(v, depth)
  if type(v) ~= 'table' then return tostring(v) end
  if depth <= 0 then return 'table' end
  local out = {}
  for k, x in pairs(v) do out[tostring(k)] = flat(x, depth - 1) end
  return out
end
local out = {}
local ok, data = pcall(DCS.getFarpsAndCarriersMissionData)
out.farps_and_carriers = ok and flat(data, 3) or tostring(data)
local ids = {}
if ok and type(data) == 'table' and type(data.farps) == 'table' then
  for id in pairs(data.farps) do ids[#ids + 1] = id end
end
for _, n in ipairs({ 'np-hg-1', 'np-hg-2' }) do
  local okc, id = E.cross('server', "local b = Airbase.getByName('" .. n .. "') return b and b:getID() or -1")
  out['airbase_id_' .. n] = okc and id or ('raised: ' .. tostring(id))
  if okc and tonumber(id) and tonumber(id) >= 0 then ids[#ids + 1] = tonumber(id) end
end
out.settings = {}
for _, id in ipairs(ids) do
  local okc, v = pcall(DCS.getDynamicSpawnSettings, id, false)
  out.settings[tostring(id)] = okc and flat(v, 2) or ('raised: ' .. tostring(v))
end
local oks, slots = pcall(DCS.getAvailableSlots, 'blue')
out.available_slots_blue = oks and type(slots) == 'table' and #slots or tostring(slots)
return out
