-- FARP fixes, after pause-read.lua: read each of the four heliports
-- spawn.lua added by StaticObject.getByName: its ID, class, isExist, life and
-- category, and the world.getAirbases entry of its name with isExist.
-- Rule: recorded as read.
-- states: server
local function try(f, ...) local ok, v = pcall(f, ...) if ok then return v end return 'raised: ' .. tostring(v) end
local out = {}
for _, n in ipairs({ 'np-ff-farp', 'np-ff-pad', 'np-ff-bare', 'np-ff-unl' }) do
  local s = StaticObject.getByName(n)
  local r = { id = s and rawget(s, 'id_'), class = s and try(function() return s.className_ end) }
  if s then
    r.exists = try(s.isExist, s); r.life = try(s.getLife, s); r.cat = try(Object.getCategory, s)
    r.dead_flag = try(function() return s:isDead() end)
  end
  for _, b in ipairs(world.getAirbases()) do
    if try(b.getName, b) == n then
      r.listed_id = rawget(b, 'id_'); r.listed_exists = try(b.isExist, b)
    end
  end
  out[n] = r
end
return out
