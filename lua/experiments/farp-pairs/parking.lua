-- FARP pairs, part 1b: where the airbase `<name>` (run.sh fills it in; `np-hg-1`
-- for the mission's pair) has its parking spots, as offsets from the airbase, and
-- which objects stand at each place of the mission's static group (a 60 m sphere,
-- every object category), to see whether the second FARP of the group stands in
-- the world and serves the one airbase.
-- Rule: recorded as read.
-- states: server
-- timeout: 30

local function try(f, ...)
  local ok, v = pcall(f, ...)
  if ok then return v end
  return 'raised: ' .. tostring(v)
end
local out = { parking = {}, near = {} }
local NAME = '<name>'
local b = try(Airbase.getByName, NAME)
if type(b) ~= 'table' then error('no airbase ' .. NAME) end
local p = try(function() return b:getPoint() end)
local spots = try(function() return b:getParking() end)
if type(spots) == 'table' then
  for i, s in ipairs(spots) do
    local v = s.vTerminalPos or {}
    local here = type(p) == 'table'
    out.parking[i] = { index = s.Term_Index, type = s.Term_Type,
      dx = here and v.x and math.floor(v.x - p.x + 0.5) or nil,
      dz = here and v.z and math.floor(v.z - p.z + 0.5) or nil }
  end
else
  out.parking = spots
end
-- The units' places, from the mission as DCS loaded it.
local places = {}
local groups = try(function() return env.mission.coalition.blue.country[1].static.group end)
if type(groups) == 'table' then
  for _, g in ipairs(groups) do
    for _, u in ipairs(g.units or {}) do
      places[#places + 1] = { name = u.name, x = u.x, y = u.y }
    end
  end
end
for _, at in ipairs(places) do
  local found = {}
  local volume = { id = world.VolumeType.SPHERE,
    params = { point = { x = at.x, y = land.getHeight({ x = at.x, y = at.y }), z = at.y }, radius = 60 } }
  local function handler(o)
    found[#found + 1] = { name = try(function() return o:getName() end),
      category = try(function() return o:getCategory() end),
      type = try(function() return o:getTypeName() end), id = rawget(o, 'id_') }
    return true
  end
  for _, cat in ipairs({ Object.Category.UNIT, Object.Category.STATIC, Object.Category.BASE,
    Object.Category.SCENERY }) do
    try(world.searchObjects, cat, volume, handler)
  end
  out.near[at.name] = found
end
return out
