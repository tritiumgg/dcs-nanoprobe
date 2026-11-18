-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): Statics added with dead = true east of the FARP, 40 m apart from 100 m, and a fuel depot at 360 m killed by an explosion.
-- states: server
-- Statics dead by script east of the FARP np#4XF1, 40 m apart from 100 m,
-- hidden = false; then a fuel depot at 360 m killed by an explosion, for
-- the ruin. Read each.
local p = Airbase.getByName('np#4XF1'):getPoint()
local types = { 'FARP Fuel Depot', 'BRDM-2', 'F-15C', 'UH-1H', 'Windsock', 'speedboat' }
local out = {}
for i, t in ipairs(types) do
  local name = 'person-fixes east ' .. i
  local z = p.z + 60 + 40 * i
  local ok, s = pcall(coalition.addStaticObject, country.id.USA,
    { name = name, type = t, x = p.x, y = z, heading = 0, dead = true, hidden = false })
  local f = StaticObject.getByName(name)
  local r = t .. ' at ' .. (60 + 40 * i) .. ' m: ' .. ((ok and s) and 'added' or ('refused ' .. tostring(s)))
  if f then
    local okl, l = pcall(f.getLife, f)
    local oke, e = pcall(f.isExist, f)
    r = r .. ', life ' .. tostring(okl and l or 'raised') .. ', isExist ' .. (oke and tostring(e) or 'raised')
  end
  out[#out + 1] = r
end
local rz = p.z + 360
coalition.addStaticObject(country.id.USA, { name = 'person-fixes east ruin', type = 'FARP Fuel Depot', x = p.x, y = rz, heading = 0, hidden = false })
trigger.action.explosion({ x = p.x, y = land.getHeight({ x = p.x, y = rz }), z = rz }, 1000)
out[#out + 1] = 'ruin depot at 360 m, exploded'
return out
