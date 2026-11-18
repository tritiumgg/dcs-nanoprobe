-- Kill the static named with an explosion at its place. Power 1,000 killed a
-- FARP Fuel Depot (LIVE, docs/results/2026-11-17-person-fixes.md). Run by
-- `cargo xtask persistence` (scenario `ruins`) in the server state through
-- the eval channel; the scenario sets NAME. `lua/tests/ruins_live.lua`
-- rehearses it on the mock.
-- states: server

local NAME = ''
local POWER = 1000

local static = StaticObject.getByName(NAME)
if not static then
  return 'none'
end
trigger.action.explosion(static:getPoint(), POWER)
return 'ok'
