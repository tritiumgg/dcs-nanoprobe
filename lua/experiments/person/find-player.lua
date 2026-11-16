-- Person tests: find the one player in an aircraft. Returns the unit's name,
-- unit ID, group ID, type and coalition, and its position. The other person
-- snippets find the player the same way.
-- states: server
-- timeout: 10

local E = ...
local found = {}
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  for _, unit in ipairs(coalition.getPlayers(side) or {}) do
    local p = unit:getPoint()
    found[#found + 1] = string.format('%s unit=%d group=%d type=%s side=%d x=%.0f z=%.0f',
      unit:getName(), unit:getID(), unit:getGroup():getID(), unit:getTypeName(), side, p.x, p.z)
  end
end
E.observe('players_in_units', found, { info = true })
return #found
