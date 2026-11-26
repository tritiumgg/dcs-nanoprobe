-- Catalog: a blue aircraft flies low over a red SA-8 and a Shilka, which
-- shoot it down. Waits for the crash and what follows: ejection, landing.
-- states: server
-- timeout: 260

local E = ...
local S = E.lib('scenario')
local R = E.lib('recorder')
R.clear()
R.start()

local origin = S.origin()
local x, z = origin.x - 6000, origin.z + 6000
R.mark('air-defense')
local sam = S.ground('np-red-ad', 'Osa 9A33 ln', {
  { name = 'np-ad-1', x = x, z = z },
}, nil, country.id.RUSSIA)
local aaa = S.ground('np-red-aaa', 'ZSU-23-4 Shilka', {
  { name = 'np-aaa-1', x = x + 100, z = z + 100 },
}, nil, country.id.RUSSIA)
E.observe('open_fire', { sam = S.open_fire(sam), aaa = S.open_fire(aaa) }, { info = true })
S.spawn_aircraft({
  name = 'np-target',
  x = x + 6000,
  z = z,
  alt = 1500,
  speed = 150,
  heading = math.pi,
  task = 'Nothing',
  points = {
    S.air_point(x + 6000, z, 150, 1500),
    S.air_point(x - 20000, z, 150, 1500),
  },
})

local started = timer.getTime()
local down
return E.wait(function()
  if not down and Unit.getByName('np-target-1') == nil then
    down = timer.getTime()
  end
  -- After the aircraft is gone, wait for the ejection and the landing.
  if (down and timer.getTime() - down >= 60) or timer.getTime() - started >= 240 then
    E.observe('catalog', R.report(), { info = true })
    -- What is left would fight on into the next scenario. A scripted
    -- removal emits no event (LIVE, lua/probes/32-events.lua).
    S.clear()
    return 'done'
  end
  return nil
end, 250, 'running')
