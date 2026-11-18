-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): B killed by an explosion of power 100.
-- states: server
local s = StaticObject.getByName('person-fixes brdm B')
local q = s:getPoint()
trigger.action.explosion({ x = q.x, y = land.getHeight({ x = q.x, y = q.z }), z = q.z }, 100)
return 'exploded'
