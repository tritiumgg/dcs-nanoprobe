-- Catalogue: two blue tanks and two red tanks 600 m apart, ordered to open
-- fire, fight until one
-- side is dead or the time is up.
-- states: server
-- timeout: 150

local E = ...
local S = E.lib('scenario')
local R = E.lib('recorder')
R.clear()
R.start()

local origin = S.origin()
local x, z = origin.x + 1500, origin.z - 1500
R.mark('combat')
local blue_group = S.ground('np-blue-tanks', 'M-1 Abrams', {
  { name = 'np-bt-1', x = x, z = z, heading = 0 },
  { name = 'np-bt-2', x = x, z = z + 40, heading = 0 },
})
local red_group = S.ground('np-red-tanks', 'T-72B', {
  { name = 'np-rt-1', x = x + 600, z = z, heading = math.pi },
  { name = 'np-rt-2', x = x + 600, z = z + 40, heading = math.pi },
}, nil, country.id.RUSSIA)
E.observe('open_fire', { blue = S.open_fire(blue_group), red = S.open_fire(red_group) }, { info = true })

local started = timer.getTime()
local ended
return E.wait(function()
  local blue = Group.getByName('np-blue-tanks')
  local red = Group.getByName('np-red-tanks')
  local over = not blue or not red or blue:getSize() == 0 or red:getSize() == 0
  if over and not ended then
    ended = timer.getTime()
  end
  -- After a side is gone, wait for the last events of the fight.
  if (ended and timer.getTime() - ended >= 10) or timer.getTime() - started >= 120 then
    E.observe('catalogue', R.report(), { info = true })
    -- What is left would fight on into the next scenario. A scripted
    -- removal emits no event (LIVE, lua/probes/32-events.lua).
    S.clear()
    return 'done'
  end
  return nil
end, 140, 'running')
