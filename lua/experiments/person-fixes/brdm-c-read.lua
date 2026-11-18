-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): C: life and isExist.
-- states: server
local out = {}
for _, n in ipairs({ 'person-fixes brdm C' }) do
  local s = StaticObject.getByName(n)
  if not s then out[#out + 1] = n .. ': not found' else
    local okl, l = pcall(s.getLife, s)
    local oke, e = pcall(s.isExist, s)
    out[#out + 1] = n .. ': life ' .. tostring(okl and l or 'raised') .. ', isExist ' .. (oke and tostring(e) or 'raised')
  end
end
return out
