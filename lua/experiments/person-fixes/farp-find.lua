-- Person-fixes, after live run 3 failed (docs/results/2026-11-17-person-fixes.md): The platform statics and airbases named np#: a spawned FARP is an airbase and is not in coalition.getStaticObjects.
-- states: server
local out = { mission = env.mission and env.mission.theatre, statics = {}, bases = {} }
for side = 0, 2 do
  for _, s in ipairs(coalition.getStaticObjects(side) or {}) do
    local n = s:getName()
    if n:sub(1, 3) == 'np#' then out.statics[#out.statics + 1] = side .. ':' .. n .. ':' .. tostring(s:getTypeName()) end
  end
end
for _, b in ipairs(world.getAirbases()) do
  local n = b:getName()
  if n:sub(1, 3) == 'np#' then out.bases[#out.bases + 1] = n .. ':' .. tostring(rawget(b, 'id_')) end
end
out.nbases = #world.getAirbases()
return out
