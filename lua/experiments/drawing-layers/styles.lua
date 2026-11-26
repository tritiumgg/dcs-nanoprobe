-- Drawing layers, line types, shapes 2, 3 (three points) and 5, and text sizes, on a
-- client. A person looks at the F10 map. Run with `cargo xtask eval <server> server
-- styles.lua`; a second run removes what the first drew. 30 to 42 km north of the
-- first airbase:
--   west: eight 6 km east-west lines (markupToAll 1), line types 0 to 7 from south to
--   north, each labeled "line type N";
--   east: markupToAll 2 with a center and a radius, markupToAll 3 with three points,
--   markupToAll 5 with one point, each labeled; then texts at sizes 8, 14 and 32.
-- IDs from 900,004,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 60

local E = ...
local B = 900004000
if E.store.styles then
  for _, id in ipairs(E.store.styles) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.styles = nil
  return { removed = true }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local BLACK, WHITE, BLUE = { 0, 0, 0, 1 }, { 1, 1, 1, 0.8 }, { 0, 0, 1, 1 }
local FILL = { 0, 0, 1, 0.4 }
local made, out = {}, {}
E.store.styles = made
local function try(name, id, f)
  local ok, err = pcall(f)
  made[#made + 1] = id
  out[#out + 1] = { name = name, id = id - B, ok = ok, err = ok and nil or tostring(err) }
end
local function label(id, x, z, text)
  try('label ' .. text, id, function()
    trigger.action.textToAll(-1, id, p(x, z), BLACK, WHITE, 12, true, text)
  end)
end
for t = 0, 7 do
  local x = 30000 + t * 1500
  try('line type ' .. t, B + t, function()
    trigger.action.markupToAll(1, -1, B + t, p(x, -24000), p(x, -18000), { 0.8, 0, 0, 1 }, FILL, t, true, '')
  end)
  label(B + 100 + t, x, -16500, 'line type ' .. t)
end
try('markup 2', B + 20, function()
  trigger.action.markupToAll(2, -1, B + 20, p(33000, -10000), 1500, BLUE, FILL, 1, true, '')
end)
label(B + 120, 35500, -10000, 'markup 2')
try('markup 3 three points', B + 30, function()
  trigger.action.markupToAll(3, -1, B + 30, p(31500, -5000), p(34500, -3500), p(31500, -2000), BLUE, FILL, 1, true, '')
end)
label(B + 130, 35500, -3500, 'markup 3, 3 points')
try('markup 5', B + 50, function()
  trigger.action.markupToAll(5, -1, B + 50, p(33000, 2000), BLUE, FILL, 14, true, 'markup 5 text')
end)
for k, size in ipairs({ 8, 14, 32 }) do
  try('text ' .. size, B + 60 + k, function()
    trigger.action.textToAll(-1, B + 60 + k, p(38000 + k * 1500, -6000), BLACK, WHITE, size, true, 'size ' .. size)
  end)
end
local held = {}
for _, panel in ipairs(world.getMarkPanels()) do
  local id = panel.idx or panel.id
  if type(id) == 'number' and id >= B and id < B + 200 then held[#held + 1] = id - B end
end
out.held = held
return out
