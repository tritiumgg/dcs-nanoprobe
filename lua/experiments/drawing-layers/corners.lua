-- Drawing layers, part C: shape 3 with two opposite corners of a 3 km square, in each
-- order, to see whether DCS draws a line or a rectangle. A person looks at the F10 map.
-- Run with `cargo xtask eval <server> server corners.lua`; a second run removes what the
-- first drew. 62 km north of the first airbase, west to east, each labeled 700 m south:
-- (SW, NE), (NE, SW), (NW, SE), (SE, NW).
-- IDs from 900,008,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 60

local E = ...
local B = 900008000
if E.store.corners then
  for _, id in ipairs(E.store.corners) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.corners = nil
  return { removed = true }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local BLUE, FILL = { 0, 0, 1, 1 }, { 0, 0, 1, 0.6 }
local made, out = {}, {}
E.store.corners = made
local function try(name, id, f)
  local ok, err = pcall(f)
  made[#made + 1] = id
  out[#out + 1] = { name = name, id = id - B, ok = ok, err = ok and nil or tostring(err) }
end
local X = 62000
local orders = {
  { 'SW,NE', function(z) return p(X, z), p(X + 3000, z + 3000) end },
  { 'NE,SW', function(z) return p(X + 3000, z + 3000), p(X, z) end },
  { 'NW,SE', function(z) return p(X + 3000, z), p(X, z + 3000) end },
  { 'SE,NW', function(z) return p(X, z + 3000), p(X + 3000, z) end },
}
for k, o in ipairs(orders) do
  local z = (k - 1) * 5000
  local a, b = o[2](z)
  try('shape 3 ' .. o[1], B + k, function()
    trigger.action.markupToAll(3, -1, B + k, a, b, BLUE, FILL, 1, true, '')
  end)
  try('label', B + 100 + k, function()
    trigger.action.textToAll(-1, B + 100 + k, p(X - 700, z), { 0, 0, 0, 1 }, { 1, 1, 1, 0.8 }, 12, true,
      '3 ' .. o[1])
  end)
end
return out
