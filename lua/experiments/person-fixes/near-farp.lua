-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): Every static within 1 km of the FARP, from the coalition lists and from a search.
-- states: server
-- Every static within 1 km of the FARP, from every coalition's list and from
-- a search: name, type, offset from the FARP, life, isExist, coalition.
local fp = Airbase.getByName('np#4XF1'):getPoint()
local seen, out = {}, {}
local function add(s, via)
  local okn, n = pcall(s.getName, s)
  n = okn and n or '?'
  if seen[n] then seen[n].via = seen[n].via .. '+' .. via return end
  local okp, p = pcall(s.getPoint, s)
  if okp and p and math.abs(p.x - fp.x) < 1000 and math.abs(p.z - fp.z) < 1000 then
    local okt, t = pcall(s.getTypeName, s)
    local okl, l = pcall(s.getLife, s)
    local oke, e = pcall(s.isExist, s)
    local okc, c = pcall(s.getCoalition, s)
    local r = { name = n, type = okt and t or 'raised', east = math.floor(p.z - fp.z + 0.5), north = math.floor(p.x - fp.x + 0.5),
      life = okl and l or 'raised', exists = oke and tostring(e) or 'raised', coalition = okc and c or 'raised', via = via }
    seen[n] = r
    out[#out + 1] = r
  end
end
for side = 0, 2 do
  for _, s in ipairs(coalition.getStaticObjects(side) or {}) do add(s, 'list' .. side) end
end
world.searchObjects(Object.Category.STATIC, { id = world.VolumeType.SPHERE, params = { point = fp, radius = 1000 } },
  function(s) add(s, 'search') return true end)
return out
