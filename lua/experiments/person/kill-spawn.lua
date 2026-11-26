-- Person tests: add one M-113 (group `np-person-kill`) DISTANCE meters in front of
-- the player's aircraft, facing it, for the damage kill in kill-damage.lua.
-- 150 m put it under a FARP pad, out of sight (LIVE, owner); 400 m clears it.
-- states: server
-- timeout: 10

local E = ...
local unit
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  unit = unit or (coalition.getPlayers(side) or {})[1]
end
if not unit then
  error('no player in an aircraft')
end
local DISTANCE = 400
local pos = unit:getPosition()
local len = math.sqrt(pos.x.x ^ 2 + pos.x.z ^ 2)
local x = pos.p.x + DISTANCE * pos.x.x / len
local z = pos.p.z + DISTANCE * pos.x.z / len
local S = E.lib('scenario')
S.ground('np-person-kill', 'M-113', { { name = 'np-person-kill-1', x = x, z = z, heading = math.atan2(-pos.x.z, -pos.x.x) } })
local spawned = Unit.getByName('np-person-kill-1')
return spawned and ('spawned id ' .. spawned:getID() .. string.format(' at x=%.0f z=%.0f', x, z)) or 'not spawned'
