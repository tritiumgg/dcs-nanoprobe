-- Loadout check: what Unit.getPlayerName and getAmmo return for the units a
-- player occupies, and what getPlayerName returns for AI units. The agent's
-- loadout read (spec/sampling.md, "Loadout reads") keeps a unit only when
-- the name is a non-empty string. Each player row gives the unit's ID, type,
-- the name's Lua type and the ammunition as `type=count` rows, the way the
-- AMMO record carries them. AI units are read up to 40, by name type.
-- states: server
-- timeout: 10

local E = ...
local function shown(v)
  return type(v) .. ':' .. tostring(v)
end

local players, seen = {}, {}
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  for _, unit in ipairs(coalition.getPlayers(side) or {}) do
    seen[unit:getID()] = true
    local rows = {}
    for _, entry in ipairs(unit:getAmmo() or {}) do
      rows[#rows + 1] = tostring(entry.desc and entry.desc.typeName) .. '=' .. tostring(entry.count)
    end
    players[#players + 1] = string.format('unit=%d type=%s name=%s ammo=[%s]', unit:getID(),
      unit:getTypeName(), shown(Unit.getPlayerName(unit)), table.concat(rows, ', '))
  end
end
E.observe('player_units', players, { info = true })

local ai, read = {}, 0
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE, coalition.side.NEUTRAL }) do
  for _, group in ipairs(coalition.getGroups(side) or {}) do
    for _, unit in ipairs(group:getUnits() or {}) do
      if read < 40 and not seen[unit:getID()] then
        read = read + 1
        local key = shown(Unit.getPlayerName(unit))
        ai[key] = (ai[key] or 0) + 1
      end
    end
  end
end
E.observe('ai_names', ai, { info = true })
E.observe('ai_read', read, { info = true })
return #players
