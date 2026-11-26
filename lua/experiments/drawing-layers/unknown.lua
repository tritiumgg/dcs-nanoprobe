-- Drawing layers, part C: ONE markupToAll call with a shape and layout of unknown
-- effect. MAY CRASH DCS: on a DCS 2.9.31.29434 dedicated server (Wine 10.0), shape 8
-- with the points layout, run alone, crashed and hung DCS inside markupToAll on
-- 2026-11-26, where pcall did not help (LIVE); shapes 9 to 12 are not probed.
-- Run each case in its own eval request, with Logs and Config copied aside and a
-- restart planned, so a crash names its exact shape and layout. A person looks at the
-- F10 map. probe.sh puts the case in:
--   <shape>   the shape number
--   <layout>  oval: (center, 1500, 800, 30, color, fill, 1, true, '')
--             points: (four corners of a 3 km square, color, fill, 1, true, '')
-- clear as the layout removes what earlier cases drew. Each case draws at 66 km north
-- of the first airbase, 5 km east per shape, with a label 700 m south.
-- IDs from 900,012,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 30

local E = ...
local SHAPE, LAYOUT = tonumber('<shape>'), '<layout>'
local B = 900012000
E.store.unknown = E.store.unknown or {}
local made = E.store.unknown
if LAYOUT == 'clear' then
  for _, id in ipairs(made) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.unknown = nil
  return { removed = #made }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local BLUE, FILL = { 0, 0, 1, 1 }, { 0, 0, 1, 0.6 }
local X, z = 66000, (SHAPE - 8) * 5000
local id = B + SHAPE * 10 + (LAYOUT == 'oval' and 1 or 2)
local ok, err
if LAYOUT == 'oval' then
  ok, err = pcall(trigger.action.markupToAll, SHAPE, -1, id, p(X + 1500, z + 1500), 1500, 800, 30, BLUE, FILL,
    1, true, '')
elseif LAYOUT == 'points' then
  ok, err = pcall(trigger.action.markupToAll, SHAPE, -1, id, p(X, z), p(X, z + 3000), p(X + 3000, z + 3000),
    p(X + 3000, z), BLUE, FILL, 1, true, '')
else
  return { error = 'unknown layout ' .. LAYOUT }
end
made[#made + 1] = id
local lid = id + 500
pcall(trigger.action.textToAll, -1, lid, p(X - 700, z + (LAYOUT == 'oval' and 0 or 2500)), { 0, 0, 0, 1 },
  { 1, 1, 1, 0.8 }, 12, true, 'shape ' .. SHAPE .. ' ' .. LAYOUT)
made[#made + 1] = lid
local panel = false
for _, m in ipairs(world.getMarkPanels()) do
  if (m.idx or m.id) == id then
    panel = true
  end
end
return { shape = SHAPE, layout = LAYOUT, id = id - B, ok = ok, err = ok and nil or tostring(err), panel = panel }
