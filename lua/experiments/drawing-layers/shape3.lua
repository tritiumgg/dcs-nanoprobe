-- CRASHES DCS. Run 3 part C ran this on a DCS 2.9.31.29434 dedicated server (Wine 10.0)
-- on 2026-11-25: one of the markupToAll calls with shapes 8 to 12 crashed DCS inside
-- the call; pcall did not catch it, and the process hung until a restart (LOG). Shape 8
-- with four points, sent alone by unknown.lua, crashes it the same way (LIVE). The
-- shape-3 half is corners.lua.
-- Drawing layers, part C: shape 3 with two corners in each order, and shapes 8 to 12.
-- A person looks at the F10 map. Run with `cargo xtask eval <server> server
-- shape3.lua`; a second run removes what the first drew. 62 km north of the first
-- airbase, west to east, each labeled 700 m south:
--   shape 3 with (SW, NE), (NE, SW), (NW, SE), (SE, NW) corners of a 3 km square;
--   shapes 8 to 12 with (center, 1500, 800, 30) and with four corner points, each under
--   pcall; a raise is reported and nothing is drawn.
-- IDs from 900,008,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 60

-- Kept as the record of the crashed run; it refuses to run. corners.lua and unknown.lua
-- (one case per run) replace it.
do
  return { error = 'superseded by corners.lua and unknown.lua: this file crashes DCS' }
end

local E = ...
local B = 900008000
if E.store.shape3 then
  for _, id in ipairs(E.store.shape3) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.shape3 = nil
  return { removed = true }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local BLUE, FILL = { 0, 0, 1, 1 }, { 0, 0, 1, 0.6 }
local made, out = {}, {}
E.store.shape3 = made
local function try(name, id, f)
  local ok, err = pcall(f)
  made[#made + 1] = id
  out[#out + 1] = { name = name, id = id - B, ok = ok, err = ok and nil or tostring(err) }
end
local function label(id, x, z, text)
  try('label', id, function()
    trigger.action.textToAll(-1, id, p(x - 700, z), { 0, 0, 0, 1 }, { 1, 1, 1, 0.8 }, 12, true, text)
  end)
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
  label(B + 100 + k, X, z, '3 ' .. o[1])
end
for s = 8, 12 do
  local z = 20000 + (s - 8) * 5000
  try('shape ' .. s .. ' oval args', B + 10 + s, function()
    trigger.action.markupToAll(s, -1, B + 10 + s, p(X + 1500, z + 1500), 1500, 800, 30, BLUE, FILL, 1, true, '')
  end)
  try('shape ' .. s .. ' points', B + 30 + s, function()
    trigger.action.markupToAll(s, -1, B + 30 + s, p(X + 4000, z), p(X + 4000, z + 3000), p(X + 7000, z + 3000),
      p(X + 7000, z), BLUE, FILL, 1, true, '')
  end)
  label(B + 120 + s, X, z, 'shape ' .. s)
end
local held = {}
for _, panel in ipairs(world.getMarkPanels()) do
  local id = panel.idx or panel.id
  if type(id) == 'number' and id >= B and id < B + 200 then held[#held + 1] = id - B end
end
out.held = held
return out
