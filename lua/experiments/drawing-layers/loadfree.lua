-- Drawing layers, does a mission load free every ID? Run it after a mission load, on
-- IDs that were removed (dead) before the load and on one never used. 20 km north of
-- the first airbase, three filled quads (shape 6) side by side, 2.5 km apart, under
-- 900,001,010, 900,001,030 and 900,009,999. The snippet reads which of the three hold
-- a panel; a person looks at the F10 map for which are drawn. A second run removes
-- them.
-- Before the load, add and remove 900,001,010 and 900,001,030 (client.lua, round 1).
-- Run with `cargo xtask eval <server> server loadfree.lua`.
-- states: server
-- timeout: 60

local E = ...
local IDS = { 900001010, 900001030, 900009999 }
if E.store.loadfree then
  for _, id in ipairs(E.store.loadfree) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.loadfree = nil
  return { removed = true }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local MAGENTA = { 1, 0, 1, 1 }
local function quad(id, z)
  trigger.action.markupToAll(6, -1, id, p(20000, z), p(20000, z + 2000), p(22000, z + 2000), p(22000, z),
    MAGENTA, { 1, 0, 1, 0.8 }, 1, true, '')
end
E.store.loadfree = IDS
for k, id in ipairs(IDS) do
  quad(id, (k - 1) * 2500)
end
local held = {}
for _, id in ipairs(IDS) do
  held[tostring(id)] = false
end
for _, panel in ipairs(world.getMarkPanels()) do
  local id = panel.idx or panel.id
  if held[tostring(id)] ~= nil then held[tostring(id)] = true end
end
return { held = held }
