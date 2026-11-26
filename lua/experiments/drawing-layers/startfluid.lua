-- Drawing layers, part C: which point moves re-fill a shape. Three 10 km bars, 1 km
-- tall, 61 to 67 km north of the first airbase, each drawn at 20% and then moved to
-- 80% length with one setter, labeled 700 m south:
--   QS: quad (shape 6) whose first point is the far top corner; setMarkupPositionStart.
--   FE: freeform (shape 7), far top corner last; setMarkupPositionEnd.
--   PE: polygon (shape 3), far top corner last; setMarkupPositionEnd.
-- Run with `cargo xtask eval <server> server startfluid.lua`; a second run removes it.
-- IDs from 900,009,000, below the platform pool (2^30 to 2^31 - 1).
-- states: server
-- timeout: 30

local E = ...
local B = 900009000
if E.store.startfluid then
  for _, id in ipairs(E.store.startfluid) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.startfluid = nil
  return { removed = true }
end
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local BLUE, FILL = { 0, 0, 1, 1 }, { 0, 0, 1, 0.8 }
local made, out = {}, {}
E.store.startfluid = made
local function run(name, f)
  local t0 = E.clock()
  local ok, err = pcall(f)
  out[#out + 1] = { name = name, ok = ok, err = ok and nil or tostring(err),
    us = math.floor((E.clock() - t0) * 1e6) }
end
local Z0 = 0
local function far(x, frac) return p(x + 1000, Z0 + 10000 * frac) end
local function farb(x, frac) return p(x, Z0 + 10000 * frac) end
local rows = { QS = 61000, FE = 64000, PE = 67000 }
-- QS: start = far top, then far bottom, near bottom, near top.
run('QS draw', function()
  local x = rows.QS
  trigger.action.markupToAll(6, -1, B + 1, far(x, 0.2), farb(x, 0.2), p(x, Z0), p(x + 1000, Z0), BLUE, FILL, 1, true, '')
end)
run('FE draw', function()
  local x = rows.FE
  trigger.action.markupToAll(7, -1, B + 2, p(x + 1000, Z0), p(x, Z0), farb(x, 0.2), far(x, 0.2), BLUE, FILL, 1, true, '')
end)
run('PE draw', function()
  local x = rows.PE
  trigger.action.markupToAll(3, -1, B + 3, p(x + 1000, Z0), p(x, Z0), farb(x, 0.2), far(x, 0.2), BLUE, FILL, 1, true, '')
end)
made[1], made[2], made[3] = B + 1, B + 2, B + 3
for k, name in ipairs({ 'QS start moved', 'FE end moved', 'PE end moved' }) do
  local x = ({ rows.QS, rows.FE, rows.PE })[k]
  trigger.action.textToAll(-1, B + 10 + k, p(x - 700, Z0), { 0, 0, 0, 1 }, { 1, 1, 1, 0.8 }, 12, true, name)
  made[#made + 1] = B + 10 + k
end
run('QS start', function() trigger.action.setMarkupPositionStart(B + 1, far(rows.QS, 0.8)) end)
run('FE end', function() trigger.action.setMarkupPositionEnd(B + 2, far(rows.FE, 0.8)) end)
run('PE end', function() trigger.action.setMarkupPositionEnd(B + 3, far(rows.PE, 0.8)) end)
return out
