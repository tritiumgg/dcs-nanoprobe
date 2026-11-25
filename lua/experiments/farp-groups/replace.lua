-- FARP groups, part 2, mission paused: the replace tries on p3, p2 and p1
-- (3 FARPs to 2, 2 to one FARP by addStaticObject, one FARP to 2), read at
-- once. `after.lua` reads them again once the simulation has run.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_groups
if not G then
  error('spawn.lua has not run')
end
local out = { airbases_before = #world.getAirbases() }
for _, tag in ipairs({ 'p3', 'p2', 'p1' }) do
  out[tag] = G.replace(tag)
end
out.airbases_after = #world.getAirbases()
return out
