-- FARP fixes, part 3 of 3: read again, in a later call than the destroys, what
-- is left of the three heliports `spawn.lua` added, then remove what remains
-- and clear the global.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local G = _G.np_farp_fixes
if not G then
  error('spawn.lua has not run')
end
local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then
    return v
  end
  return 'raised: ' .. tostring(v)
end
local function call(f, ...)
  local ok, v = pcall(f, ...)
  return ok and ('returned ' .. tostring(v)) or ('raised: ' .. tostring(v))
end

local out = { airbases = #world.getAirbases(), objects = {}, cleanup = {} }
for _, name in ipairs(G.names) do
  local st, ab0 = G.statics[name], G.bases[name]
  local listed, live_listed = 0, 0
  for _, b in ipairs(world.getAirbases()) do
    if try(b.getName, b) == name then
      listed = listed + 1
      if try(b.isExist, b) == true then
        live_listed = live_listed + 1
      end
    end
  end
  local in_statics = false
  for side = 0, 2 do
    for _, o in ipairs(coalition.getStaticObjects(side) or {}) do
      if try(o.getName, o) == name then
        in_statics = true
      end
    end
  end
  local sid = st and rawget(st, 'id_')
  out.objects[name] = {
    static_by_name = StaticObject.getByName(name) ~= nil,
    airbase_by_name = Airbase.getByName(name) ~= nil,
    old_static_exists = st and try(st.isExist, st),
    old_static_life = st and try(st.getLife, st),
    old_airbase_exists = ab0 and try(ab0.isExist, ab0),
    exists_by_id = sid and try(Object.isExist, { id_ = sid }),
    in_get_airbases = listed, existing_in_get_airbases = live_listed,
    in_get_static_objects = in_statics,
  }
  local left = StaticObject.getByName(name)
  if left then
    out.cleanup[name .. ':static'] = call(left.destroy, left)
  end
  local base = Airbase.getByName(name)
  if base then
    out.cleanup[name .. ':airbase'] = call(Object.destroy, base)
  end
end
_G.np_farp_fixes = nil
return out
