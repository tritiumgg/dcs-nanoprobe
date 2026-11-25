-- FARP fixes: whether `coalition.addStaticObject` reads a helipad's frequency
-- given as text, the form a saved file holds, with a value that is not the
-- default 127.5 MHz. It reads the airbase's frequency and modulation, then
-- destroys the helipad (DCS takes the destroy when the simulation next runs).
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local anchor
for _, base in ipairs(world.getAirbases()) do
  local ok, d = pcall(base.getDesc, base)
  if not anchor and ok and type(d) == 'table' and d.category == 0 then
    anchor = base
  end
end
if not anchor then
  error('no airdrome')
end
local p = anchor:getPoint()
local out = {}
for i, f in ipairs({ { 'np-ff-text', '131.25', 1 }, { 'np-ff-num', 131.25, 0 } }) do
  local ok, made = pcall(coalition.addStaticObject, country.id.USA, {
    category = 'Heliports', type = 'SINGLE_HELIPAD', shape_name = 'FARP', name = f[1],
    x = p.x + 3200 + 300 * i, y = p.z, heading = 0,
    heliport_callsign_id = 4, heliport_frequency = f[2], heliport_modulation = f[3],
  })
  local r = { added = ok and made ~= nil }
  local ab = Airbase.getByName(f[1])
  if ab then
    local comm = ab:getCommunicator()
    r.frequency = select(2, pcall(comm.getFrequency, comm))
    r.modulation = select(2, pcall(comm.getModulation, comm))
    r.callsign = select(2, pcall(ab.getCallsign, ab))
  end
  local s = StaticObject.getByName(f[1])
  r.destroy = s and tostring(pcall(s.destroy, s)) or 'none'
  out[f[1]] = r
end
return out
