-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): A Shelter added dead at 650 m; .Ammunition depot, refused as a type name.
-- states: server
local p = Airbase.getByName('np#4XF1'):getPoint()
local out = {}
for _, d in ipairs({ { 'person-fixes shelter', 'Shelter', 650 }, { 'person-fixes ammo', '.Ammunition depot', 750 } }) do
  local ok, s = pcall(coalition.addStaticObject, country.id.USA,
    { name = d[1], type = d[2], x = p.x, y = p.z + d[3], heading = 0, dead = true, hidden = false })
  local f = StaticObject.getByName(d[1])
  local r = d[2] .. ' at ' .. d[3] .. ' m: ' .. ((ok and s) and 'added' or ('refused ' .. tostring(s)))
  if f then
    local okl, l = pcall(f.getLife, f)
    local oke, e = pcall(f.isExist, f)
    r = r .. ', life ' .. tostring(okl and l or 'raised') .. ', isExist ' .. (oke and tostring(e) or 'raised')
  end
  out[#out + 1] = r
end
return out
