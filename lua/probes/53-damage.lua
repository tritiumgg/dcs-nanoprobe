-- Damage is set from the mission state: a_unit_set_life_percentage takes a
-- mission ID and changes what Unit:getLife() returns in the server state.
-- states: hook
-- timeout: 60

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

cross('server', [[
  local S = E.lib('scenario')
  local origin = S.origin()
  S.ground('np-dmg', 'M-113', { { name = 'np-dmg-1', x = origin.x - 400, z = origin.z + 300 } })
  return 'spawned'
]])

local start = DCS.getModelTime()
local id, set_at

return E.wait(function()
  local now = DCS.getModelTime()
  if not id then
    if now - start < 1 then
      return nil
    end
    id = tonumber(cross('server', [[return tonumber(Unit.getByName('np-dmg-1'):getID())]]))
    E.observe('life_before', tonumber(cross('server', [[return Unit.getByName('np-dmg-1'):getLife()]])),
      { windows = 3, within = 0.2 })
    cross('mission', 'a_unit_set_life_percentage(' .. id .. ', 40) return "set"')
    set_at = now
    return nil
  end
  if now - set_at < 1 then
    return nil
  end
  -- 40 % of an M-113's life of 3 is 1.2 (LIVE).
  E.observe('life_after', tonumber(cross('server', [[return Unit.getByName('np-dmg-1'):getLife()]])),
    { windows = 1.2, within = 0.05 })
  cross('server', [[E.lib('scenario').destroy('np-dmg') return 'gone']])
  return 'done'
end, 30, 'running')
