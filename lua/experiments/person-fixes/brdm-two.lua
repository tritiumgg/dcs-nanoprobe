-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): BRDM-2 A added dead at 400 m, B added alive at 460 m.
-- states: server
local p = Airbase.getByName('np#4XF1'):getPoint()
local function read(n)
  local s = StaticObject.getByName(n)
  if not s then return n .. ': not found' end
  local okl, l = pcall(s.getLife, s)
  local oke, e = pcall(s.isExist, s)
  return n .. ': life ' .. tostring(okl and l or 'raised') .. ', isExist ' .. (oke and tostring(e) or 'raised')
end
coalition.addStaticObject(country.id.USA, { name = 'person-fixes brdm A', type = 'BRDM-2', x = p.x, y = p.z + 400, heading = 0, dead = true, hidden = false })
coalition.addStaticObject(country.id.USA, { name = 'person-fixes brdm B', type = 'BRDM-2', x = p.x, y = p.z + 460, heading = 0, hidden = false })
return { read('person-fixes brdm A'), read('person-fixes brdm B') }
