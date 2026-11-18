-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): A spawned FARP read as a static and as an airbase: IDs, isExist, life, coalition. The FARP name is the run's.
-- states: server
local n = 'np#eIQ1'
local out = {}
local ok, s = pcall(StaticObject.getByName, n)
out.static_found = ok and s ~= nil
if ok and s then
  out.static_id = rawget(s, 'id_')
  local oke, e = pcall(s.isExist, s) out.exists = oke and e or 'raised'
  local okl, l = pcall(s.getLife, s) out.life = okl and l or 'raised'
  local okc, c = pcall(s.getCategory, s) out.category = okc and c or 'raised'
end
local b = Airbase.getByName(n)
out.airbase_id = b and rawget(b, 'id_')
local okc, c = pcall(b.getCoalition, b) out.coalition = okc and c or 'raised'
local okd, d = pcall(b.getDesc, b) out.desc_category = okd and d and d.category
return out
