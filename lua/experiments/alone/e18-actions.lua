-- E18, part one: what a connected player sees of the platform's actions. Run
-- with `cargo xtask eval <server target> hook` while one client sits in an
-- aircraft. It spawns a burst of 200 ground units 1.5 km north of the first
-- player aircraft it finds (or of the scenario origin, when none is found), sets one unit's life to 10%, adds a radio menu
-- item for blue, puts a mark on the F10 map, and shows a message. Everything
-- is removed at reset (`cargo xtask reset <target>`).
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

-- Where the player is: x north, z east.
local X, Z = cross('server', [[
  for _, side in ipairs({ coalition.side.BLUE, coalition.side.RED }) do
    local unit = coalition.getPlayers(side)[1]
    if unit then
      local p = unit:getPoint()
      return string.format('%d %d', p.x, p.z)
    end
  end
  local o = E.lib('scenario').origin()
  return string.format('%d %d', o.x, o.z)
]]):match('(%S+) (%S+)')
X, Z = tonumber(X), tonumber(Z)

E.observe('burst', cross('server', string.format([[
  local S = E.lib('scenario')
  local t0 = E.clock()
  local groups = 0
  for g = 0, 19 do
    local units = {}
    for u = 1, 10 do
      units[u] = { name = string.format('np-e18-%%d', g * 10 + u),
        x = %d + 1500 + (g %% 5) * 60, z = %d - 300 + math.floor(g / 5) * 150 + u * 12 }
    end
    S.ground(string.format('np-e18-g%%d', g + 1), 'M-113', units)
    groups = groups + 1
  end
  return string.format('%%d groups in %%.1f ms', groups, (E.clock() - t0) * 1000)
]], X, Z)), { info = true })

local id = tonumber(cross('server', [[return tonumber(Unit.getByName('np-e18-1'):getID())]]))
cross('mission', 'a_unit_set_life_percentage(' .. id .. ', 10) return "set"')
E.observe('damaged_unit', 'np-e18-1, mission id ' .. id, { info = true })

E.observe('menu_and_mark', cross('server', string.format([[
  E.track.global('np_e18', { pressed = 0 })
  local path = missionCommands.addCommandForCoalition(coalition.side.BLUE, 'nanoprobe test item', nil,
    function() np_e18.pressed = np_e18.pressed + 1 end)
  E.track.defer('menu', function() missionCommands.removeItemForCoalition(coalition.side.BLUE, path) end)
  local point = { x = %d + 1300, y = 0, z = %d }
  trigger.action.markToAll(91801, 'nanoprobe test mark', point, true)
  E.track.defer('mark', function() trigger.action.removeMark(91801) end)
  trigger.action.outText('nanoprobe test: 200 units north of the ramp, a mark, and a radio item', 60)
  return 'menu, mark and message added'
]], X, Z)), { info = true })
return 'done'
