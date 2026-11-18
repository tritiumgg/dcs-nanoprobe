-- Person-fixes live run 4 (docs/results/2026-11-17-person-fixes.md): CLEAR_DEBRIS's dead-static step by hand on the exploded depot: collect in searchObjects, destroy after it.
-- states: server
-- The fixed CLEAR_DEBRIS predicate on the ruin: life 0, isExist false or
-- raising, name not np#; collected in searchObjects, destroyed after it.
local s0 = StaticObject.getByName('person-fixes east ruin')
local p = s0:getPoint()
local volume = { id = world.VolumeType.SPHERE, params = { point = p, radius = 30 } }
local dead, seen = {}, {}
world.searchObjects(Object.Category.STATIC, volume, function(s)
  local got, life = pcall(s.getLife, s)
  local known, exists = pcall(s.isExist, s)
  local named, name = pcall(s.getName, s)
  seen[#seen + 1] = tostring(name) .. ' life ' .. tostring(life) .. ' isExist ' .. (known and tostring(exists) or 'raised')
  if got and type(life) == 'number' and life <= 0 and (not known or exists == false)
    and named and type(name) == 'string' and name:sub(1, 3) ~= 'np#' then
    dead[#dead + 1] = s
  end
  return true
end)
local destroyed = 0
for _, s in ipairs(dead) do
  if pcall(s.destroy, s) then destroyed = destroyed + 1 end
end
local after = StaticObject.getByName('person-fixes east ruin')
return { seen = seen, destroyed = destroyed, found_after = after ~= nil }
