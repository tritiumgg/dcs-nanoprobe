-- Drawing layers, part C: fluid bars whose moving end is the FIRST point, grown with
-- setMarkupPositionStart, which re-fills the shape (LIVE, session 2). A person looks at
-- the F10 map and screenshots after each step. run: sed the step in for <step>:
--   draw   two bars 10 km long, 1.5 km high, at 10%, x 70,000 and 72,500 north of the
--          first airbase, z 0 to 10,000, each labeled 700 m south:
--          SQ quad (shape 6): first point the far top corner, then near top, near
--          bottom, far bottom; the far bottom stays at 10%
--          SR freeform capsule (shape 7, 16 points per end arc): first point the far
--          arc's apex; the rest of the far arc stays at 10%
--   25|50|75  one setMarkupPositionStart per bar, the first point to that percent
--   clear  remove both bars and labels
-- Every call is timed in microseconds. IDs from 900,010,000, below the platform pool.
-- states: server
-- timeout: 30

local E = ...
local STEP = '<step>'
local B = 900010000
local S = E.store.startbar or { n = 0, made = {} }
E.store.startbar = S
local clock = E.clock() and E.clock or os.clock
local at = world.getAirbases()[1]:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local LEN, H, R = 10000, 1500, 750
local BLUE, FILL, BLACK, WHITE = { 0, 0, 1, 1 }, { 0, 0, 1, 0.8 }, { 0, 0, 0, 1 }, { 1, 1, 1, 0.6 }
local X = { SQ = 70000, SR = 72500 }
local out = { step = STEP, timings = {} }
local function T(name, f, ...)
  local t0 = clock()
  local ok, err = pcall(f, ...)
  out.timings[#out.timings + 1] = { call = name, us = math.floor((clock() - t0) * 1e6 + 0.5),
    error = not ok and tostring(err) or nil }
end
local function nid()
  S.n = S.n + 1
  S.made[#S.made + 1] = B + S.n
  return B + S.n
end
local function len(pct)
  return LEN * pct / 100
end
-- The far arc's apex of the capsule at `pct`.
local function apex(pct)
  return p(X.SR + R, len(pct))
end

if STEP == 'clear' then
  for _, id in ipairs(S.made) do
    pcall(trigger.action.removeMark, id)
  end
  E.store.startbar = nil
  out.removed = #S.made
  return out
elseif STEP == 'draw' then
  if S.SQ then
    error('drawn: run clear first')
  end
  local l = len(10)
  local x = X.SQ
  S.SQ = nid()
  T('markupToAll SQ', trigger.action.markupToAll, 6, -1, S.SQ, p(x + H, l), p(x + H, 0), p(x, 0), p(x, l),
    BLACK, FILL, 1, true, '')
  -- Clockwise round the outline from the far apex: a point at angle t (0 north, pi/2
  -- east) on an end arc is (xc + R cos t, zc + R sin t). Far arc t = pi/2 to pi, near
  -- arc pi to 2 pi, far arc 0 to pi/2; 16 points per arc.
  local xc = X.SR + R
  local function arc(zc, t0, t1, n, first)
    local pts = {}
    for i = first, n do
      local t = t0 + (t1 - t0) * i / n
      pts[#pts + 1] = p(xc + R * math.cos(t), zc + R * math.sin(t))
    end
    return pts
  end
  local pts = { apex(10) }
  local farz = l - R
  for _, part in ipairs({
    arc(farz, math.pi / 2, math.pi, 8, 1),
    arc(R, math.pi, 2 * math.pi, 15, 0),
    arc(farz, 0, math.pi / 2, 8, 0),
  }) do
    for _, pt in ipairs(part) do
      pts[#pts + 1] = pt
    end
  end
  table.remove(pts) -- the apex again
  S.SR = nid()
  local args = { 7, -1, S.SR }
  for _, pt in ipairs(pts) do
    args[#args + 1] = pt
  end
  for _, v in ipairs({ BLACK, FILL, 1, true, '' }) do
    args[#args + 1] = v
  end
  T('markupToAll SR', trigger.action.markupToAll, unpack(args))
  out.sr_points = #pts
  for name, x in pairs(X) do
    pcall(trigger.action.textToAll, -1, nid(), p(x - 700, 0), BLACK, WHITE, 14, true,
      name .. (name == 'SQ' and ' quad, first point far top' or ' capsule, first point far apex'))
  end
  return out
end
local pct = tonumber(STEP)
if not pct or not S.SQ then
  error('no step ' .. STEP .. ' (or draw first)')
end
T('setMarkupPositionStart SQ', trigger.action.setMarkupPositionStart, S.SQ, p(X.SQ + H, len(pct)))
T('setMarkupPositionStart SR', trigger.action.setMarkupPositionStart, S.SR, apex(pct))
return out
