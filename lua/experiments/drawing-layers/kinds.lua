-- Drawing layers, shape kind against ID and coalition audience, on a client. A person
-- looks at the F10 map. Run with `cargo xtask eval <server> server kinds.lua`; a second
-- run removes what the first drew. 22 km north of the first airbase, west of the
-- airbase's longitude:
--   a filled quad (shape 6) under the LOWER ID with a filled circle under the HIGHER ID
--   over it, the quad added after the circle: by ID the circle is on top; by creation
--   or by "quads over circles" the quad is on top;
--   two filled quads side by side, east of it: the west one for coalition 1 (red), the
--   east one for coalition 2 (blue), each labeled.
-- IDs from 900,003,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 60

local E = ...
local B = 900003000
if E.store.kinds then
  for _, id in ipairs(E.store.kinds) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.kinds = nil
  return { removed = true }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local function quad(coalition, id, x, z, size, color)
  trigger.action.markupToAll(6, coalition, id, p(x, z), p(x, z + size), p(x + size, z + size), p(x + size, z),
    color, { color[1], color[2], color[3], 0.8 }, 1, true, '')
end
local made = { B + 90, B + 10, B + 20, B + 30, B + 21, B + 31 }
E.store.kinds = made
-- Circle first, under the higher ID; then the quad, under the lower ID, over it.
trigger.action.circleToAll(-1, B + 90, p(24000, -26000), 2500, { 1, 0, 0, 1 }, { 1, 0, 0, 0.8 }, 1, true, '')
quad(-1, B + 10, 22000, -28000, 4000, { 0, 0, 1, 1 })
-- Coalition audiences: red-only west, blue-only east, labels for all.
quad(1, B + 20, 22000, -18000, 3000, { 1, 0.5, 0, 1 })
quad(2, B + 30, 22000, -13000, 3000, { 0, 0.6, 0, 1 })
trigger.action.textToAll(-1, B + 21, p(26000, -18000), { 0, 0, 0, 1 }, { 1, 1, 1, 0.8 }, 14, true, 'red only')
trigger.action.textToAll(-1, B + 31, p(26000, -13000), { 0, 0, 0, 1 }, { 1, 1, 1, 0.8 }, 14, true, 'blue only')
local held = {}
for _, panel in ipairs(world.getMarkPanels()) do
  local id = panel.idx or panel.id
  if type(id) == 'number' and id >= B and id < B + 100 then held[#held + 1] = id - B end
end
return { held = held }
