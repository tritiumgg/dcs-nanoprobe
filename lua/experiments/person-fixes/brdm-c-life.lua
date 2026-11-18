-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): C given life 0 with a_unit_set_life_percentage, the DAMAGE setter, through the mission state.
-- states: hook
local v = net.dostring_in('server', "local s = StaticObject.getByName('person-fixes brdm C') return s and s:getID() or ''")
local id = tonumber(v)
if not id then return 'no static: ' .. tostring(v) end
local r, ok = net.dostring_in('mission', 'a_unit_set_life_percentage(' .. string.format('%d', id) .. ', 0) return "called"')
return { id = id, result = tostring(r), ok = tostring(ok) }
