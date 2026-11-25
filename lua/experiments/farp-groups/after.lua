-- FARP groups, part 5, mission paused after `run.lua`: read every group again,
-- whether the old objects of the replace tries still exist, the airbase count,
-- and the events the handler recorded.
-- Rule: recorded as read.
-- states: server
-- timeout: 60

local G = _G.np_farp_groups
if not G then
  error('spawn.lua has not run')
end
local out = { airbases = #world.getAirbases(), groups = {}, old = {} }
for _, tag in ipairs(G.order) do
  out.groups[tag] = G.read(tag, math.max(#G.groups[tag].units, 3))
end
for tag, old in pairs(G.old or {}) do
  local ok, v = pcall(function() return old:isExist() end)
  out.old[tag] = { id = rawget(old, 'id_'), exists = ok and v or ('raised: ' .. tostring(v)) }
end
out.running = G.running
out.events = G.events
return out
