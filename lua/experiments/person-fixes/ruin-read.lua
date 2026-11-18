-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): The exploded fuel depot's life and isExist.
-- states: server
local s = StaticObject.getByName('person-fixes east ruin')
if not s then return 'not found' end
local okl, l = pcall(s.getLife, s)
local oke, e = pcall(s.isExist, s)
return 'life ' .. tostring(okl and l or 'raised') .. ', isExist ' .. (oke and tostring(e) or 'raised')
