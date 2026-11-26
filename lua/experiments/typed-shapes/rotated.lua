-- Typed shapes: the DCS calls the typed lowering makes for rotated shapes, a circle and a
-- radius change. A person looks at the F10 map and screenshots after the steps marked (shot).
-- Run with `run.sh <server> <step>`.
--   draw    (shot) row 1, x 70,000: R0 a 3 km by 1.5 km rectangle as a quad (shape 6) at
--           heading 0; R30 the same at heading 30 degrees; QK a skewed quad (no right angle);
--           F30 the R30 corners as a 4-point freeform (shape 7), the fallback.
--           Row 2, x 76,000: OV an oval 3 km by 1.5 km (half-axes 1,500 and 750) at heading
--           30 degrees, 36 points (shape 7); RR a rounded rectangle 3 km by 1.5 km, corner
--           400 m, heading 30 degrees, 40 points; RC the same with corner 750 m (half the
--           width: shared arc end points sent once, 38 points); CI a circleToAll of 1,500 m.
--   radius  (shot) setMarkupRadius on CI to 2,500 m
--   clear   remove everything this file made and start a new round
-- Points go round the outline in one direction; each point is rounded to 1 mm, as the
-- sidecar's lowering will. Heading is radians clockwise from north, of the length axis.
-- Every call is timed in microseconds. Items stand 6 km apart in z from the first airbase;
-- each label is a text 700 m south of its item's center.
-- states: server
-- timeout: 60

local E = ...
local STEP = '<step>'
-- IDs from 900,030,000, below the platform pool (2^30 to 2^31 - 1). Each clear starts a
-- new round with its own 1,000 IDs: DCS never draws a removed ID again in a process.
local S = E.store.typed or { round = 0, n = 0, made = {}, held = {} }
E.store.typed = S
if not S.deferred then
  S.deferred = true
  E.track.defer('typed-shapes rotated', function()
    for id in pairs(S.made) do
      pcall(trigger.action.removeMark, id)
    end
  end)
end
if S.round >= 60 then
  error('out of rounds: the ID span 900,030,000 to 900,089,999 is used up')
end
local B = 900030000 + 1000 * S.round
local function nid()
  S.n = S.n + 1
  if S.n >= 1000 then
    error('the round is out of IDs: run clear')
  end
  return B + S.n
end

local base = world.getAirbases()[1]
local at = base:getPoint()
local function mm(v)
  return math.floor(v * 1000 + 0.5) / 1000
end
local function p(dn, de)
  return { x = mm(at.x + dn), y = 0, z = mm(at.z + de) }
end
local BLUE, RED, GREEN, BLACK = { 0, 0, 1, 1 }, { 1, 0, 0, 1 }, { 0, 0.6, 0, 1 }, { 0, 0, 0, 1 }
local function fill(c)
  return { c[1], c[2], c[3], 0.6 }
end

-- A point a along the length axis and b across it, from center (cn, ce) at heading h.
local function local_pt(cn, ce, h, a, b)
  return p(cn + a * math.cos(h) - b * math.sin(h), ce + a * math.sin(h) + b * math.cos(h))
end
local function rect(cn, ce, len, wid, h)
  local l, w = len / 2, wid / 2
  return {
    local_pt(cn, ce, h, l, w), local_pt(cn, ce, h, -l, w),
    local_pt(cn, ce, h, -l, -w), local_pt(cn, ce, h, l, -w),
  }
end
local function oval(cn, ce, hl, hw, h)
  local pts = {}
  for k = 0, 35 do
    local t = math.rad(10 * k)
    pts[#pts + 1] = local_pt(cn, ce, h, hl * math.cos(t), hw * math.sin(t))
  end
  return pts
end
-- Four 90-degree arcs in 10-degree steps, 10 points each; a point equal to the one before
-- it (or the last equal to the first) is sent once.
local function rounded(cn, ce, len, wid, r, h)
  local cl, cw = len / 2 - r, wid / 2 - r
  local centers = { { cl, cw }, { -cl, cw }, { -cl, -cw }, { cl, -cw } }
  local pts = {}
  local function same(a, b)
    return a.x == b.x and a.z == b.z
  end
  for q, c in ipairs(centers) do
    for k = 0, 9 do
      local t = math.rad(90 * (q - 1) + 10 * k)
      local pt = local_pt(cn, ce, h, c[1] + r * math.cos(t), c[2] + r * math.sin(t))
      if #pts == 0 or not same(pts[#pts], pt) then
        pts[#pts + 1] = pt
      end
    end
  end
  if #pts > 1 and same(pts[#pts], pts[1]) then
    pts[#pts] = nil
  end
  return pts
end

local clock = E.clock() and E.clock or os.clock
local out = { step = STEP, airbase = base:getName(), round = S.round, timings = {} }
local function T(name, f, ...)
  local t0 = clock()
  local ok, err = pcall(f, ...)
  local row = { call = name, us = math.floor((clock() - t0) * 1e6 + 0.5) }
  if not ok then
    row.error = tostring(err)
  end
  out.timings[#out.timings + 1] = row
  return ok
end
local function markup(name, shape, pts, color)
  local id = nid()
  S.made[id] = true
  S.held[name] = id
  local args = { shape, -1, id }
  for _, pt in ipairs(pts) do
    args[#args + 1] = pt
  end
  for _, v in ipairs({ color, fill(color), 1, true, name }) do
    args[#args + 1] = v
  end
  T(name .. ' shape ' .. shape .. ' (' .. #pts .. ' points)', trigger.action.markupToAll, unpack(args))
end
local function label(cn, ce, text)
  local id = nid()
  S.made[id] = true
  T('label ' .. text, trigger.action.textToAll, -1, id, p(cn - 700, ce), BLACK, { 1, 1, 1, 0.8 }, 12, true, text)
end

local H = math.rad(30)
if STEP == 'draw' then
  if S.drawn then
    error('run clear before a second draw')
  end
  S.drawn = true
  local X1, X2 = 70000, 76000
  markup('R0', 6, rect(X1, 0, 3000, 1500, 0), BLUE)
  label(X1 - 1000, 0, 'R0 quad 0 deg')
  markup('R30', 6, rect(X1, 6000, 3000, 1500, H), BLUE)
  label(X1 - 1000, 6000, 'R30 quad 30 deg')
  markup('QK', 6, { p(X1 - 800, 11000), p(X1 + 1200, 12400), p(X1 + 600, 14300), p(X1 - 1100, 13600) }, RED)
  label(X1 - 1000, 12500, 'QK skewed quad')
  markup('F30', 7, rect(X1, 18000, 3000, 1500, H), GREEN)
  label(X1 - 1000, 18000, 'F30 freeform 4 pts 30 deg')
  markup('OV', 7, oval(X2, 0, 1500, 750, H), BLUE)
  label(X2 - 1000, 0, 'OV oval 30 deg')
  markup('RR', 7, rounded(X2, 6000, 3000, 1500, 400, H), RED)
  label(X2 - 1000, 6000, 'RR rounded 30 deg')
  markup('RC', 7, rounded(X2, 12000, 3000, 1500, 750, H), GREEN)
  label(X2 - 1000, 12000, 'RC capsule 30 deg')
  local id = nid()
  S.made[id] = true
  S.held.CI = id
  T('CI circleToAll', trigger.action.circleToAll, -1, id, p(X2, 18000), 1500, BLUE, fill(BLUE), 1, true, 'CI')
  label(X2 - 1800, 18000, 'CI circle')
elseif STEP == 'radius' then
  if not S.held.CI then
    error('run draw first')
  end
  T('CI setMarkupRadius 2500', trigger.action.setMarkupRadius, S.held.CI, 2500)
elseif STEP == 'clear' then
  for id in pairs(S.made) do
    pcall(trigger.action.removeMark, id)
  end
  S.made, S.held, S.n, S.drawn = {}, {}, 0, nil
  S.round = S.round + 1
  out.cleared = true
else
  error('unknown step: ' .. STEP)
end

local ids, panels = {}, {}
for name, id in pairs(S.held) do
  ids[#ids + 1] = { name = name, id = id - B }
end
table.sort(ids, function(a, b) return a.id < b.id end)
out.held = ids
for _, m in ipairs(world.getMarkPanels() or {}) do
  -- DCS lists `idx`; the mock lists `id`.
  local id = m.idx or m.id
  if type(id) == 'number' and id >= B and id < B + 1000 then
    panels[#panels + 1] = id - B
  end
end
table.sort(panels)
out.panels = panels
return out
