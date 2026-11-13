-- Catalogue: aircraft at the first airdrome. One takes off from the runway,
-- one starts cold from parking, and a helicopter lands.
-- states: server
-- timeout: 300

local E = ...
local S = E.lib('scenario')
local R = E.lib('recorder')
R.clear()
R.start()

local base = S.airdrome()
if not base then
  E.observe('airdrome', false, { info = true })
  return 'done'
end
local away = S.air_point(base.x + 30000, base.z + 30000, 200, 3000)
-- The three aircraft act at the same time, so each one's events are filed
-- under its own cause by its unit name.
R.mark_unit('np-runway', 'runway')
R.mark_unit('np-cold', 'cold')
R.mark_unit('np-heli', 'land')

R.mark('runway')
S.spawn_aircraft({
  name = 'np-runway',
  x = base.x,
  z = base.z,
  alt = base.y,
  speed = 0,
  task = 'Nothing',
  points = { S.airdrome_point('runway', base), away },
})

-- A parking spot for a fighter: an open spot or a shelter that is free.
local spot
for _, p in ipairs(base.base:getParking() or {}) do
  if (p.Term_Type == 104 or p.Term_Type == 72) and not p.TO_AC then
    spot = p
    break
  end
end
E.observe('parking_found', spot ~= nil, { info = true })
if spot then
  R.mark('cold')
  local cold = S.airdrome_point('cold', base)
  cold.x, cold.y = spot.vTerminalPos.x, spot.vTerminalPos.z
  S.spawn_aircraft({
    name = 'np-cold',
    x = spot.vTerminalPos.x,
    z = spot.vTerminalPos.z,
    alt = base.y,
    speed = 0,
    task = 'Nothing',
    parking = spot.Term_Index,
    points = { cold, away },
  })
end

R.mark('land')
S.spawn_aircraft({
  name = 'np-heli',
  kind = 'UH-1H',
  category = Group.Category.HELICOPTER,
  x = base.x + 2500,
  z = base.z + 2500,
  alt = 300,
  speed = 50,
  fuel = 400,
  gun = 100,
  task = 'Transport',
  points = {
    S.air_point(base.x + 2500, base.z + 2500, 50, 300),
    S.airdrome_point('land', base, 30, 0),
  },
})

local started = timer.getTime()
return E.wait(function()
  local landed = R.count('land', 'S_EVENT_LAND') > 0
  local cold_off = not spot or R.count('cold', 'S_EVENT_TAKEOFF') > 0
  if (landed and cold_off and timer.getTime() - started >= 60) or timer.getTime() - started >= 270 then
    E.observe('catalogue', R.report(), { info = true })
    -- What is left would fight on into the next scenario. A scripted
    -- removal emits no event (LIVE, lua/probes/32-events.lua).
    S.clear()
    return 'done'
  end
  return nil
end, 290, 'running')
