-- An airbase changes coalition from a script once auto-capture is off. The
-- probe puts the airbase back as it found it.
-- states: server

local E = ...
local base = world.getAirbases()[1]
local before = base:getCoalition()
local target = before == coalition.side.RED and coalition.side.BLUE or coalition.side.RED

E.track.defer('airbase', function()
  base:setCoalition(before)
  base:autoCapture(true)
end)

base:autoCapture(false)
base:setCoalition(target)
-- Airbase:autoCapture(false) and setCoalition changed an airbase from 0 to
-- 1 (LIVE).
E.observe('coalition_before', before, { info = true })
E.observe('coalition_changed', base:getCoalition() == target, { windows = true })

base:setCoalition(before)
base:autoCapture(true)
E.observe('coalition_restored', base:getCoalition() == before, { info = true })
