-- Warehouse evidence: a third FARP added during the mission, 6 km north of
-- Dwyer, with the dynamic-spawn flags and `hidden = false` set explicitly, to
-- see whether a client's F10 map shows a scripted FARP when the field is given
-- (np-wh-farp and np-wh-farp2, added without it, are not shown; FARP London-1,
-- from the editor with `hidden = false` in its group, is).
-- Rule: recorded as the person at the client reports it.
-- states: server
-- timeout: 30

local E = ...
local p = Airbase.getByName('Dwyer'):getPoint()
local ok, err = pcall(coalition.addStaticObject, country.id.USA, {
  category = 'Heliports', type = 'FARP', name = 'np-wh-farp3', x = p.x + 6000, y = p.z,
  heading = 0, heliport_callsign_id = 3, heliport_frequency = 129.5, heliport_modulation = 0,
  dynamicSpawn = true, allowHotStart = true, dynamicCargo = true, hidden = false,
})
local b = Airbase.getByName('np-wh-farp3')
E.observe('add', { ok = ok, error = (not ok) and tostring(err) or nil, airbase = b ~= nil,
  id = b and tonumber(b:getID()) or -1 }, { info = true })
return 'done'
