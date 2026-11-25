-- FARP groups, last part: remove the event handler, destroy every object the
-- groups still hold by any unit name, and clear the global. A reload of the
-- mission (`lua/experiments/farp-fixes/reload.lua`) follows.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_groups
if not G then
  return 'nothing to clean'
end
world.removeEventHandler(G.handler)
local n = 0
for _, tag in ipairs(G.order) do
  for k = 1, 10 do
    local st = StaticObject.getByName('np-fg-' .. tag .. '-' .. k)
    if st and pcall(st.isExist, st) and st:isExist() then
      pcall(st.destroy, st)
      n = n + 1
    end
    local un = Unit.getByName('np-fg-' .. tag .. '-' .. k)
    if un and pcall(un.isExist, un) and un:isExist() then
      pcall(un.destroy, un)
      n = n + 1
    end
  end
end
_G.np_farp_groups = nil
return { destroyed = n }
