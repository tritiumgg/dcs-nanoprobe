-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): BRDM-2 C added alive at 520 m.
-- states: server
local p = Airbase.getByName('np#4XF1'):getPoint()
local s = coalition.addStaticObject(country.id.USA, { name = 'person-fixes brdm C', type = 'BRDM-2', x = p.x, y = p.z + 520, heading = 0, hidden = false })
return s and s:getID() or 'none'
