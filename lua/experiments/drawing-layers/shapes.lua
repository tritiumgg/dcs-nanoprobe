-- Drawing layers, shape IDs: what each markupToAll shape ID draws on a client, beside
-- rectToAll. A person looks at the F10 map; the server reads the panels. Run with
-- `cargo xtask eval <server> server shapes.lua`; a second run removes what the first
-- drew. Five 3 km cells in a row west to east, 10 km north and 24 km east of the
-- first airbase, each with a text label 1 km north of it:
--   rectToAll(a, b); markupToAll 1 (a, b); markupToAll 3 (a, b); markupToAll 4 (a, b);
--   markupToAll 6 (four corners).
-- On a client rectToAll draws a filled rectangle, markupToAll 1 and 3 with two points
-- a line, 4 an arrow, and 6 with four corners a filled quad (LIVE, run 3 part B).
-- IDs from 900,002,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 60

local E = ...
local B = 900002000
local base = world.getAirbases()[1]
local at = base:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local BLUE, BLACK = { 0, 0, 1, 1 }, { 0, 0, 0, 1 }
local FILL = { 0, 0, 1, 0.5 }

if E.store.shapes then
  for _, id in ipairs(E.store.shapes) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.shapes = nil
  return { removed = true }
end

local made = {}
E.track.defer('drawing-layers shapes', function()
  for _, id in ipairs(made) do
    pcall(trigger.action.removeMark, id)
  end
end)
local out = {}
local function try(name, id, f)
  local ok, err = pcall(f)
  made[#made + 1] = id
  out[#out + 1] = { name = name, id = id, ok = ok, err = ok and nil or tostring(err) }
end
local function label(i, text)
  local id = B + 100 + i
  try('label ' .. text, id, function()
    trigger.action.textToAll(-1, id, p(10000 + 4000, 24000 + i * 5000), BLACK, { 1, 1, 1, 0.8 }, 14, true, text)
  end)
end
local cells = {
  { 'rectToAll', function(id, a, b) trigger.action.rectToAll(-1, id, a, b, BLUE, FILL, 1, true, '') end },
  { 'markup 1', function(id, a, b) trigger.action.markupToAll(1, -1, id, a, b, BLUE, FILL, 1, true, '') end },
  { 'markup 3', function(id, a, b) trigger.action.markupToAll(3, -1, id, a, b, BLUE, FILL, 1, true, '') end },
  { 'markup 4', function(id, a, b) trigger.action.markupToAll(4, -1, id, a, b, BLUE, FILL, 1, true, '') end },
  { 'markup 6', function(id, a, b, c, d) trigger.action.markupToAll(6, -1, id, a, c, b, d, BLUE, FILL, 1, true, '') end },
}
for i, cell in ipairs(cells) do
  local z = 24000 + (i - 1) * 5000
  local a, b = p(10000, z), p(13000, z + 3000)
  local c, d = p(10000, z + 3000), p(13000, z)
  try(cell[1], B + i, function() cell[2](B + i, a, b, c, d) end)
  label(i - 1, cell[1])
end
E.store.shapes = made
local panels = {}
for _, panel in ipairs(world.getMarkPanels()) do
  local id = panel.idx or panel.id
  if type(id) == 'number' and id >= B and id < B + 200 then
    panels[#panels + 1] = id - B
  end
end
out.panels = panels
return out
