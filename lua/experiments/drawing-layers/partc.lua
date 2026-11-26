-- Drawing layers, part C: which setMarkup* calls exist, what each one does to a
-- drawing a client already shows, and what a progress bar costs. A person looks at
-- the F10 map and screenshots after the steps marked (shot); run.sh puts the step in.
--   list     every key of trigger.action and world that contains 'ark', sorted, with its
--            type, and every ...ToAll name (a LIVE enumeration of what exists)
--   draw     (shot) row 1 five filled quads Q1..Q5 (shape 6), row 2 a filled rectToAll R1,
--            row 3 a lineToAll L1 and a markupToAll 1 line L2, row 4 a filled freeform F1
--            (shape 7, five points) and a filled polygon P1 (shape 3, four points)
--   move     (shot) Q1..Q4 setMarkupPositionEnd, the last corner 2 km east; Q5
--            setMarkupPositionStart, the first corner 2 km west; R1, L1 and L2 end 2 km
--            east; F1 and P1 start 2 km south
--   refresh  (shot) the follow-up calls: Q1 setMarkupColorFill, Q2 setMarkupColor, Q3
--            setMarkupTypeLine, Q4 setMarkupText, Q5 setMarkupColorFill, R1, F1 and P1
--            setMarkupColorFill, each with the value it already has
--   other    every setMarkup* in `list` the steps above do not use, tried with pcall on a
--            circle C1 drawn here (a radius setter, any other), the results reported
--   bars     (shot) three progress bars, 10 km long, each labeled: SEG (10 adjacent quad
--            cells, fill alpha 0 is empty), FLUID (one quad; both far vertices moved with
--            setMarkupPositionStart and End, then setMarkupColorFill) and READD (one
--            rectToAll at 0% length)
--   sweep    over about 10 s of real time step all three bars from 0 to 100% in 10% steps,
--            one step a second, 11 steps: SEG sets fill alpha 0.8 on cell k; FLUID moves
--            and refreshes; READD adds a rectToAll under a fresh ID, then removes the old
--            one, in the same call. Returns each step's cost per bar and min/median/max.
--   shapes2  (shot) shape 2 under four argument layouts with pcall (which raise is reported),
--            ovals as freeforms of 16, 32, 64 and 36 points and a 32-point polygon (shape 3),
--            rounded rectangles as freeforms of 4, 8, 16 and 9 points per corner arc
--   rbars    (shot) progress bars 10 km long, z 13,000 to 23,000: FLAT (rectToAll), RND-C
--            (rectangle body and two end circles, fill alpha 1.0), RND-P (one freeform,
--            16 points per end arc)
--   rsweep   like sweep for FLAT (End moved), RND-C (body End moved, right circle re-added
--            under a fresh ID) and RND-P (re-added whole); a rounded bar is at least one
--            bar height (1.5 km) long, so 0% draws that. After step 10 one
--            setMarkupPositionEnd on the RND-P freeform, reported
--   rhalf    (shot) after rbars, one step of each rounded bar to 50%, as rsweep's step 5
--   clear    remove everything this file made and start a new round
-- Every call to DCS is timed with the best real clock in microseconds, and the reply lists
-- the panel IDs this file holds.
-- Layout, from the first airbase, x north and z east, rows 40 to 60 km north:
--   row 1 (Q1..Q5)  x 40,000, z 0, 5,000, 10,000, 15,000, 20,000, each 2 km square
--   row 2 (R1)      x 43,500, z 0, 2 km by 3 km;  C1 (other) x 43,500, z 12,000, 1.5 km radius
--   row 3 (L1, L2)  x 47,000, L1 z 0 to 3,000, L2 z 6,000 to 9,000
--   row 4 (F1, P1)  x 50,500, F1 z 0 to 4,000, P1 z 8,000 to 12,000
--   bars            x 53,500 SEG, 56,000 FLUID, 58,500 READD, z 0 to 10,000, 1.5 km high
--   shapes2         x 40,000 ovals and 43,500 rounded rects, z 27,000 + 5,000 per item; shape 2
--                   tries at x 48,000 (z 27,000 to 42,000)
--   rbars           x 53,500 FLAT, 56,000 RND-C, 58,500 RND-P, z 13,000 to 23,000
-- Each label is a text 700 m south of its item.
-- states: server
-- timeout: 60

local E = ...
local STEP = '<step>'
-- IDs from 900,020,000, below the platform pool (2^30 to 2^31 - 1). Each clear starts a
-- new round with its own 1,000 IDs: DCS never draws a removed ID again.
local S = E.store.partc or { round = 0, n = 0, made = {}, held = {} }
E.store.partc = S
local made, held = S.made, S.held
if not S.deferred then
  S.deferred = true
  E.track.defer('drawing-layers partc', function()
    for id in pairs(made) do
      pcall(trigger.action.removeMark, id)
    end
  end)
end
if S.round >= 80 then
  error('out of rounds: the ID span 900,020,000 to 900,099,999 is used up')
end
local B = 900020000 + 1000 * S.round
local function nid()
  S.n = S.n + 1
  if S.n >= 1000 then
    error('the round is out of IDs: run clear')
  end
  return B + S.n
end

local base = world.getAirbases()[1]
local at = base:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local RED, GREEN, BLUE, YELLOW, BLACK = { 1, 0, 0, 1 }, { 0, 0.8, 0, 1 }, { 0, 0, 1, 1 }, { 1, 1, 0, 1 }, { 0, 0, 0, 1 }
local WHITE = { 1, 1, 1, 1 }
local function fill(c, a)
  return { c[1], c[2], c[3], a or 0.6 }
end

local clock = E.clock() and E.clock or os.clock
local out = { step = STEP, airbase = base:getName(), round = S.round, clock = E.clock() and 'E.clock' or 'os.clock', timings = {} }

-- Call f under pcall and time it in microseconds; the row goes to `sink`.
local function T(sink, name, f, ...)
  local t0 = clock()
  local ok, err = pcall(f, ...)
  local row = { call = name, us = math.floor((clock() - t0) * 1e6 + 0.5) }
  if not ok then
    row.error = tostring(err)
  end
  sink[#sink + 1] = row
  return ok
end

local function need_drawn()
  if not S.drawn then
    error('run draw first (and clear before a second draw)')
  end
end

local function label(x, z, text)
  local id = nid()
  pcall(trigger.action.textToAll, -1, id, p(x - 700, z), BLACK, fill(WHITE, 0.6), 14, true, text)
  made[id] = true
end

-- Add a drawing: argsfn(id) returns the call's arguments. The ID is fresh and held by name.
local function add(name, call, argsfn, sink)
  local id = nid()
  T(sink or out.timings, call, trigger.action[call], unpack(argsfn(id)))
  made[id] = true
  held[name] = id
  return id
end
-- Add under a fresh ID, then remove the held one, in the same call.
local function swap(name, call, argsfn, sink)
  local old = held[name]
  add(name, call, argsfn, sink)
  T(sink or out.timings, 'removeMark', trigger.action.removeMark, old)
  made[old] = nil
end
local function set(name, call, value, sink)
  T(sink or out.timings, call, trigger.action[call], held[name], value)
end

-- A quad's ring: the first corner is the south-west one, the last is the south-east one.
local function quad(x, z, h, w)
  return p(x, z), p(x + h, z), p(x + h, z + w), p(x, z + w)
end

local function stats(list)
  local sorted = {}
  for i, v in ipairs(list) do
    sorted[i] = v
  end
  table.sort(sorted)
  local n = #sorted
  if n == 0 then
    return nil
  end
  return { n = n, min = sorted[1], median = sorted[math.floor((n + 1) / 2)], max = sorted[n] }
end

local function finish()
  local ids = {}
  for _, panel in ipairs(world.getMarkPanels()) do
    local id = panel.idx or panel.id
    if type(id) == 'number' and id >= B and id < B + 1000 then
      ids[#ids + 1] = id
    end
  end
  table.sort(ids)
  out.panels = ids
  out.held = held
  return out
end


local COL = { Q1 = RED, Q2 = GREEN, Q3 = BLUE, Q4 = YELLOW, Q5 = { 1, 0, 1, 1 }, R1 = BLUE, F1 = GREEN, P1 = RED }
local Q = { 'Q1', 'Q2', 'Q3', 'Q4', 'Q5' }
local USED = { setMarkupPositionStart = true, setMarkupPositionEnd = true, setMarkupColor = true, setMarkupColorFill = true,
  setMarkupTypeLine = true, setMarkupFontSize = true, setMarkupText = true }
local function qz(i)
  return 5000 * (i - 1)
end
local function F1_points()
  local x = 50500
  return { p(x, 0), p(x, 4000), p(x + 2000, 4000), p(x + 3000, 2000), p(x + 2000, 0) }
end
local function P1_points()
  local x = 50500
  return { p(x, 8000), p(x, 12000), p(x + 2000, 12000), p(x + 2000, 8000) }
end

-- Bars: x of each bar's south edge, its height, and its length.
local BAR_X = { SEG = 53500, FLUID = 56000, READD = 58500, FLAT = 53500, RNDC = 56000, RNDP = 58500 }
local BAR_H, BAR_LEN, RBAR_Z = 1500, 10000, 13000
local RAD = BAR_H / 2
local function fluid_len(k)
  return math.max(1, k * BAR_LEN / 10)
end
-- FLUID's ring starts and ends on the far edge, so Start and End move that edge.
local function fluid_ring(len)
  local x = BAR_X.FLUID
  return p(x + BAR_H, len), p(x + BAR_H, 0), p(x, 0), p(x, len)
end
-- A rounded bar is at least as long as it is high.
local function round_len(k)
  return math.max(BAR_H, k * BAR_LEN / 10)
end
local BARFILL = fill(BLUE, 0.8)
local SOLID = fill(BLUE, 1.0)

-- A freeform capsule from z 0 to len at x, `m` points on each end arc.
local function capsule(x, z0, len, m)
  local pts = {}
  for i = 0, m - 1 do
    local a = -math.pi / 2 + math.pi * i / (m - 1)
    pts[#pts + 1] = p(x + RAD + RAD * math.sin(a), z0 + len - RAD + RAD * math.cos(a))
  end
  for i = 0, m - 1 do
    local a = math.pi / 2 + math.pi * i / (m - 1)
    pts[#pts + 1] = p(x + RAD + RAD * math.sin(a), z0 + RAD + RAD * math.cos(a))
  end
  return pts
end
local function ellipse(cx, cz, ax, az, n)
  local pts = {}
  for i = 0, n - 1 do
    local a = 2 * math.pi * i / n
    pts[#pts + 1] = p(cx + ax * math.sin(a), cz + az * math.cos(a))
  end
  return pts
end
-- A rounded rectangle, h north by w east from (x, z), corner radius r, m points per arc.
local function rounded(x, z, h, w, r, m)
  local pts = {}
  local corners = {
    { x + h - r, z + w - r, 0 }, { x + r, z + w - r, 90 }, { x + r, z + r, 180 }, { x + h - r, z + r, 270 },
  }
  for _, c in ipairs(corners) do
    for i = 0, m - 1 do
      local a = math.rad(c[3] + 90 * i / (m - 1))
      pts[#pts + 1] = p(c[1] + r * math.cos(a), c[2] + r * math.sin(a))
    end
  end
  return pts
end
local function free(shape, id, pts, color, fillc, text)
  local args = { shape, -1, id }
  for _, pt in ipairs(pts) do
    args[#args + 1] = pt
  end
  args[#args + 1] = color
  args[#args + 1] = fillc
  args[#args + 1] = 1
  args[#args + 1] = true
  args[#args + 1] = text or ''
  return args
end

-- Step the bars through 0 to 100% in 10% steps, once a second in real time. Each bar is
-- { name, step = function(k, sink) }.
local b_after
local function sweep(bars)
  local steps = {}
  for _, b in ipairs(bars) do
    steps[b.name] = {}
  end
  local t0, k = clock(), 0
  return E.wait(function()
    local due = math.floor(clock() - t0)
    while k <= 10 and k <= due do
      for _, b in ipairs(bars) do
        local sink = {}
        b.step(k, sink)
        local total = 0
        for _, r in ipairs(sink) do
          total = total + r.us
        end
        steps[b.name][k + 1] = { pct = k * 10, t_ms = math.floor((clock() - t0) * 1000), us = total, calls = sink }
      end
      k = k + 1
    end
    if k <= 10 then
      return nil
    end
    out.timings = nil
    out.steps = steps
    out.step_count = k
    out.cost = {}
    for _, b in ipairs(bars) do
      local totals = {}
      for i = 2, 11 do
        totals[#totals + 1] = steps[b.name][i].us
      end
      out.cost[b.name] = { step0_us = steps[b.name][1].us, steps1to10_us = stats(totals) }
    end
    if b_after then
      b_after()
    end
    return finish()
  end, 25)
end

if STEP == 'list' then
  local function names(t, pattern)
    local list = {}
    pcall(function()
      for name, value in pairs(t) do
        if type(name) == 'string' and name:find(pattern) then
          list[#list + 1] = name .. ' ' .. type(value)
        end
      end
    end)
    table.sort(list)
    return list
  end
  out.trigger_action = names(trigger.action, 'ark')
  out.world = names(world, 'ark')
  out.drawing = names(trigger.action, 'ToAll')
  out.timings = nil
  return finish()
elseif STEP == 'draw' then
  if S.drawn then
    error('already drawn: run clear first')
  end
  S.drawn = true
  for i, name in ipairs(Q) do
    local a, b, c, d = quad(40000, qz(i), 2000, 2000)
    add(name, 'markupToAll', function(id)
      return { 6, -1, id, a, b, c, d, COL[name], fill(COL[name]), 1, true, name }
    end)
    label(40000, qz(i), name .. ' quad shape 6')
  end
  add('R1', 'rectToAll', function(id)
    return { -1, id, p(43500, 0), p(45500, 3000), COL.R1, fill(COL.R1), 1, true, 'R1' }
  end)
  label(43500, 0, 'R1 rectToAll')
  add('L1', 'lineToAll', function(id)
    return { -1, id, p(47000, 0), p(47000, 3000), RED, 1, true, 'L1' }
  end)
  label(47000, 0, 'L1 lineToAll')
  add('L2', 'markupToAll', function(id)
    return { 1, -1, id, p(47000, 6000), p(47000, 9000), BLUE, fill(BLUE), 1, true, 'L2' }
  end)
  label(47000, 6000, 'L2 markupToAll 1')
  add('F1', 'markupToAll', function(id)
    return free(7, id, F1_points(), COL.F1, fill(COL.F1), 'F1')
  end)
  label(50500, 0, 'F1 freeform shape 7')
  add('P1', 'markupToAll', function(id)
    return free(3, id, P1_points(), COL.P1, fill(COL.P1), 'P1')
  end)
  label(50500, 8000, 'P1 polygon shape 3')
elseif STEP == 'move' then
  need_drawn()
  for i = 1, 4 do
    set(Q[i], 'setMarkupPositionEnd', p(40000, qz(i) + 4000))
  end
  set('Q5', 'setMarkupPositionStart', p(40000, qz(5) - 2000))
  set('R1', 'setMarkupPositionEnd', p(45500, 5000))
  set('L1', 'setMarkupPositionEnd', p(47000, 5000))
  set('L2', 'setMarkupPositionEnd', p(47000, 11000))
  set('F1', 'setMarkupPositionStart', p(48500, 0))
  set('P1', 'setMarkupPositionStart', p(48500, 8000))
elseif STEP == 'refresh' then
  need_drawn()
  set('Q1', 'setMarkupColorFill', fill(COL.Q1))
  set('Q2', 'setMarkupColor', COL.Q2)
  set('Q3', 'setMarkupTypeLine', 1)
  set('Q4', 'setMarkupText', 'Q4')
  set('Q5', 'setMarkupColorFill', fill(COL.Q5))
  set('R1', 'setMarkupColorFill', fill(COL.R1))
  set('F1', 'setMarkupColorFill', fill(COL.F1))
  set('P1', 'setMarkupColorFill', fill(COL.P1))
elseif STEP == 'other' then
  if not held.C1 then
    add('C1', 'circleToAll', function(id)
      return { -1, id, p(43500, 12000), 1500, RED, fill(RED), 1, true, 'C1' }
    end)
    label(43500, 12000, 'C1 circleToAll')
  end
  local names = {}
  pcall(function()
    for name, value in pairs(trigger.action) do
      if type(name) == 'string' and name:find('^setMarkup') and type(value) == 'function' and not USED[name] then
        names[#names + 1] = name
      end
    end
  end)
  table.sort(names)
  out.unused = names
  out.tried = {}
  for _, name in ipairs(names) do
    local arg, kind
    if name:find('Radius') then
      arg, kind = 3000, 'number 3000'
    elseif name:find('Pos') or name:find('Point') then
      arg, kind = p(43500, 12500), 'point'
    elseif name:find('Color') then
      arg, kind = RED, 'color'
    elseif name:find('Text') then
      arg, kind = 'C1 changed', 'text'
    else
      arg, kind = 1, 'number 1'
    end
    local sink = {}
    local ok = T(sink, name, trigger.action[name], held.C1, arg)
    out.tried[#out.tried + 1] = { call = name, arg = kind, ok = ok, us = sink[1].us, error = sink[1].error }
  end
  out.timings = nil
elseif STEP == 'bars' then
  if S.bars then
    error('bars are drawn: run clear first')
  end
  S.bars = true
  for k = 1, 10 do
    local z = 1000 * (k - 1)
    local a, b, c, d = quad(BAR_X.SEG, z, BAR_H, 1000)
    add('SEG' .. k, 'markupToAll', function(id)
      return { 6, -1, id, a, b, c, d, BLACK, fill(BLUE, 0), 1, true, '' }
    end)
  end
  label(BAR_X.SEG, 0, 'SEG 10 quad cells')
  local a, b, c, d = fluid_ring(fluid_len(0))
  add('FLUID', 'markupToAll', function(id)
    return { 6, -1, id, a, b, c, d, BLACK, BARFILL, 1, true, '' }
  end)
  label(BAR_X.FLUID, 0, 'FLUID one quad, edge moved')
  add('READD', 'rectToAll', function(id)
    return { -1, id, p(BAR_X.READD, 0), p(BAR_X.READD + BAR_H, 1), BLACK, BARFILL, 1, true, '' }
  end)
  label(BAR_X.READD, 0, 'READD rect, re-added')
elseif STEP == 'sweep' then
  if not S.bars then
    error('run bars first')
  end
  return sweep({
    { name = 'SEG', step = function(k, sink)
      if k == 0 then
        for c = 1, 10 do
          set('SEG' .. c, 'setMarkupColorFill', fill(BLUE, 0), sink)
        end
      else
        set('SEG' .. k, 'setMarkupColorFill', fill(BLUE, 0.8), sink)
      end
    end },
    { name = 'FLUID', step = function(k, sink)
      local a, _, _, d = fluid_ring(fluid_len(k))
      set('FLUID', 'setMarkupPositionStart', a, sink)
      set('FLUID', 'setMarkupPositionEnd', d, sink)
      set('FLUID', 'setMarkupColorFill', BARFILL, sink)
    end },
    { name = 'READD', step = function(k, sink)
      swap('READD', 'rectToAll', function(id)
        return { -1, id, p(BAR_X.READD, 0), p(BAR_X.READD + BAR_H, fluid_len(k)), BLACK, BARFILL, 1, true, '' }
      end, sink)
    end },
  })
elseif STEP == 'shapes2' then
  if S.shapes2 then
    error('shapes2 are drawn: run clear first')
  end
  S.shapes2 = true
  -- Shape 2 under several argument layouts; a raise is recorded, a person sees what draws.
  local c = p(48000, 27000)
  local layouts = {
    { 'center, point', { c, p(48000, 29000) } },
    { 'center, two radii', { c, 1000, 2000 } },
    { 'two points', { p(47000, 27000), p(49000, 31000) } },
    { 'center, radius', { c, 1500 } },
  }
  out.shape2 = {}
  for i, l in ipairs(layouts) do
    local name = 'S2' .. i
    local z = 27000 + 5000 * (i - 1)
    local args = l[2]
    local sink = {}
    local id = nid()
    -- Move each layout's own geometry east so the four do not overlap.
    local shifted = {}
    for j, v in ipairs(args) do
      shifted[j] = type(v) == 'table' and { x = v.x, y = 0, z = v.z + 5000 * (i - 1) } or v
    end
    local call = { 2, -1, id }
    for _, v in ipairs(shifted) do
      call[#call + 1] = v
    end
    for _, v in ipairs({ BLUE, fill(BLUE), 1, true, name }) do
      call[#call + 1] = v
    end
    local ok = T(sink, 'markupToAll', trigger.action.markupToAll, unpack(call))
    made[id] = true
    held[name] = id
    out.shape2[#out.shape2 + 1] = { layout = l[1], id = id, ok = ok, error = sink[1].error, us = sink[1].us }
    label(48000, z, name .. ' shape 2: ' .. l[1])
  end
  -- Ovals as freeforms (shape 7) of 16, 32, 64 and 36 points (the mission editor's
  -- circleSides_, ED-CODE), and a 32-point one as polygon (shape 3).
  local ovals = { { 'O16', 7, 16 }, { 'O32', 7, 32 }, { 'O64', 7, 64 }, { 'O32P', 3, 32 }, { 'O36', 7, 36 } }
  for i, o in ipairs(ovals) do
    local z = 27000 + 5000 * (i - 1)
    add(o[1], 'markupToAll', function(id)
      return free(o[2], id, ellipse(41000, z + 2000, 1000, 2000, o[3]), BLACK, fill(BLUE), '')
    end)
    label(40000, z, o[1] .. ' oval ' .. o[3] .. ' points, shape ' .. o[2])
  end
  -- Rounded rectangles as freeforms of 4, 8, 16 and 9 points per corner arc (9 is 36
  -- per full turn, as the editor's circles).
  for i, m in ipairs({ 4, 8, 16, 9 }) do
    local z = 27000 + 5000 * (i - 1)
    add('RR' .. m, 'markupToAll', function(id)
      return free(7, id, rounded(43500, z, 2000, 4000, 700, m), BLACK, fill(GREEN), '')
    end)
    label(43500, z, 'RR' .. m .. ' rounded rect, ' .. m .. ' points per arc')
  end
elseif STEP == 'rbars' then
  if S.rbars then
    error('rbars are drawn: run clear first')
  end
  S.rbars = true
  local x = BAR_X.FLAT
  add('FLAT', 'rectToAll', function(id)
    return { -1, id, p(x, RBAR_Z), p(x + BAR_H, RBAR_Z + BAR_LEN), BLACK, BARFILL, 1, true, '' }
  end)
  label(x, RBAR_Z, 'FLAT rectToAll, full 100%')
  -- RND-C at 100%: a body and a circle at each end, all with fill alpha 1.0.
  local xc = BAR_X.RNDC
  add('RNDC_body', 'rectToAll', function(id)
    return { -1, id, p(xc, RBAR_Z + RAD), p(xc + BAR_H, RBAR_Z + BAR_LEN - RAD), BLUE, SOLID, 1, true, '' }
  end)
  add('RNDC_L', 'circleToAll', function(id)
    return { -1, id, p(xc + RAD, RBAR_Z + RAD), RAD, BLUE, SOLID, 1, true, '' }
  end)
  add('RNDC_R', 'circleToAll', function(id)
    return { -1, id, p(xc + RAD, RBAR_Z + BAR_LEN - RAD), RAD, BLUE, SOLID, 1, true, '' }
  end)
  label(xc, RBAR_Z, 'RND-C body and two circles, alpha 1.0, full')
  local xp = BAR_X.RNDP
  add('RNDP', 'markupToAll', function(id)
    return free(7, id, capsule(xp, RBAR_Z, BAR_LEN, 16), BLUE, BARFILL, '')
  end)
  label(xp, RBAR_Z, 'RND-P freeform, 16 points per end arc, full')
elseif STEP == 'rsweep' or STEP == 'rhalf' then
  if not S.rbars then
    error('run rbars first')
  end
  local xc, xp, x = BAR_X.RNDC, BAR_X.RNDP, BAR_X.FLAT
  -- After step 10, one setMarkupPositionEnd on the freeform: its last point goes 2 km south.
  b_after = function()
    local sink = {}
    set('RNDP', 'setMarkupPositionEnd', p(xp - 2000, RBAR_Z + RAD), sink)
    out.rndp_position_end = { to = 'x - 2000 at the last point (south of the near end)', call = sink[1] }
  end
  local bars = {
    { name = 'FLAT', step = function(k, sink)
      set('FLAT', 'setMarkupPositionEnd', p(x + BAR_H, RBAR_Z + round_len(k)), sink)
    end },
    { name = 'RND-C', step = function(k, sink)
      local len = round_len(k)
      -- Body: its end corner moves (the body is empty width at 0%).
      set('RNDC_body', 'setMarkupPositionEnd', p(xc + BAR_H, RBAR_Z + math.max(RAD + 1, len - RAD)), sink)
      -- Circles have no position setter we know of: the right one is re-added.
      swap('RNDC_R', 'circleToAll', function(id)
        return { -1, id, p(xc + RAD, RBAR_Z + len - RAD), RAD, BLUE, SOLID, 1, true, '' }
      end, sink)
    end },
    { name = 'RND-P', step = function(k, sink)
      swap('RNDP', 'markupToAll', function(id)
        return free(7, id, capsule(xp, RBAR_Z, round_len(k), 16), BLUE, BARFILL, '')
      end, sink)
    end },
  }
  if STEP == 'rsweep' then
    return sweep(bars)
  end
  -- rhalf: one step of each bar to 50%, as rsweep's step 5, and stop there.
  out.half = {}
  for _, bar in ipairs(bars) do
    local sink = {}
    bar.step(5, sink)
    out.half[bar.name] = sink
  end
elseif STEP == 'clear' then
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
    made[id] = nil
  end
  for name in pairs(held) do
    held[name] = nil
  end
  S.round = S.round + 1
  S.n = 0
  S.drawn, S.bars, S.shapes2, S.rbars = nil, nil, nil, nil
  out.round = S.round
  out.timings = nil
else
  error('no step ' .. STEP)
end
return finish()
