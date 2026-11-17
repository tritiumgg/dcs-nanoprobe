-- Loadout check: one AI ground unit is born, so a recording of the agent's
-- records can show that an AI birth gives no AMMO record. Spawns an M-113
-- 1 km north of the first player unit (or of the map origin with no player),
-- waits 5 s of model time (resuming the simulation if it is paused) for the
-- birth and the pumps after it, then destroys the group. Returns the unit's ID.
-- states: server
-- timeout: 30

local E = ...
local at = { x = 0, z = 0 }
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  local unit = (coalition.getPlayers(side) or {})[1]
  if unit then
    at = unit:getPoint()
    break
  end
end
local name = 'np-loadout-ai'
coalition.addGroup(country.id.USA, Group.Category.GROUND, {
  name = name, task = 'Ground Nothing', visible = false, start_time = 0,
  x = at.x + 1000, y = at.z,
  units = { { name = name .. '-1', type = 'M-113', x = at.x + 1000, y = at.z, heading = 0, skill = 'Average' } },
  route = { points = {} },
})
local unit = Unit.getByName(name .. '-1')
E.observe('ai_unit', unit and unit:getID() or 'none', { info = true })
E.observe('ai_player_name', unit and tostring(Unit.getPlayerName(unit)) or 'none', { info = true })
local t0 = timer.getTime()
return E.wait(function()
  if timer.getTime() - t0 < 5 then
    return nil
  end
  local g = Group.getByName(name)
  if g then
    g:destroy()
  end
  return unit and unit:getID() or 0
end, 20, 'running')
