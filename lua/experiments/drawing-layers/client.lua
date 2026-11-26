-- Drawing layers, client check: what a client draws on top. A person looks at
-- the F10 map and reports after each step; run.sh puts the step in.
--   place    draw two stacks of overlapping filled shapes beside the first airbase,
--            each with a creation order that is not its ID order
--   readd    remove the lowest drawing of stack A and add it again under its ID
--   newid    remove stack A's red and add it again under a lower ID
--   over     add over stack A's blue with no remove: a yellow quad
--   flicker  for 10 s, once a second, remove stack B's line (ID 51) and add a filled
--            quad under a fresh ID in the same call
--   inplace  stack B's line (its quad after flicker) and text: color, fill color,
--            line type and font size changed in place with the four setters; the
--            text's text too
--   geometry stack B's line (its quad after flicker): far point moved with
--            setMarkupPositionEnd; the freeform's first point moved 3 km south with
--            setMarkupPositionStart
--   highids  four lines in a row 10 km south, west to east: IDs 2^31 - 1, 2^31,
--            2^32 and 2^53 - 1; the person says which they see
--   watch    record every mark event from now on (a player's marks)
--   read     return the marks recorded since watch, and the panels
--   clear    remove every drawing this file made
-- Stack A, three lines (shape 3 with two points draws a line on a client, not a
-- rectangle), 2 km apart: red ID 30, created first; green ID 10, second;
-- blue ID 20, third. Drawn by ID, blue is on top of green and red on top of both;
-- by creation, blue is on top of both.
-- Stack B, four shapes of different kinds: a circle ID 52 first, a freeform
-- ID 50 second, a line ID 51 third, a text ID 53 fourth.
-- states: server
-- timeout: 60

local E = ...
local STEP = '<step>'
-- IDs from 900,000,000, below the platform pool (2^30 to 2^31 - 1).
-- Each clear starts a new round with its own 1,000 IDs: DCS never draws a removed ID again.
local B = 900000000 + 1000 + 1000 * (E.store.round or 0)
E.store.made = E.store.made or {}
local made = E.store.made
if not E.store.deferred then
  E.store.deferred = true
  E.track.defer('drawing-layers client', function()
    for id in pairs(made) do
      pcall(trigger.action.removeMark, id)
    end
  end)
end

local base = world.getAirbases()[1]
local at = base:getPoint()
local function p(dx, dz)
  return { x = at.x + dx, y = 0, z = at.z + dz }
end
local function corners(x, z, size)
  return p(x, z), p(x + size, z + size)
end
local RED, GREEN, BLUE, YELLOW = { 1, 0, 0, 1 }, { 0, 0.8, 0, 1 }, { 0, 0, 1, 1 }, { 1, 1, 0, 1 }
local function fill(c)
  return { c[1], c[2], c[3], 0.8 }
end

-- Shape 3 with two corners: a client draws a line from corner to corner.
local function rect(id, x, z, color, text)
  local a, b = corners(x, z, 4000)
  trigger.action.markupToAll(3, -1, B + id, a, b, color, fill(color), 1, true, text or '')
  made[B + id] = true
end
local function remove(id)
  trigger.action.removeMark(B + id)
  made[B + id] = nil
end

local out = { step = STEP, airbase = base:getName() }
-- Our drawings in the order getMarkPanels lists them, by offset from B.
local function finish()
  local order = {}
  for _, panel in ipairs(world.getMarkPanels()) do
    local id = panel.idx or panel.id
    if type(id) == 'number' and id >= B and id < B + 100 then
      order[#order + 1] = id - B
    end
  end
  out.order = order
  return out
end
if STEP == 'place' then
  -- Stack A, 10 km north.
  rect(30, 10000, 0, RED)
  rect(10, 12000, 2000, GREEN)
  rect(20, 14000, 4000, BLUE)
  -- Stack B, 10 km north and 12 km east: circle, freeform, line.
  trigger.action.circleToAll(-1, B + 52, p(12000, 14000), 2500, RED, fill(RED), 1, true, '')
  made[B + 52] = true
  trigger.action.markupToAll(7, -1, B + 50, p(10000, 12000), p(10000, 16000), p(14000, 16000), p(15000, 14000),
    p(14000, 12000), GREEN, fill(GREEN), 1, true, '')
  made[B + 50] = true
  rect(51, 11000, 13000, BLUE)
  trigger.action.markupToAll(5, -1, B + 53, p(11000, 17000), BLUE, fill(BLUE), 14, true, 'np 53')
  made[B + 53] = true
elseif STEP == 'readd' then
  -- Read the panels on the server: is there one under the ID, with the text, and
  -- how many of ours are listed. Right after the re-add, and again after 2 s of
  -- real time; the snippet never resumes a paused server.
  local function read()
    local r = { exists = false, count = 0 }
    for _, panel in ipairs(world.getMarkPanels()) do
      local id = panel.idx or panel.id
      if type(id) == 'number' and id >= B and id < B + 100 then
        r.count = r.count + 1
        if id == B + 10 then
          r.exists = true
          r.text = panel.text
          r.text_ok = panel.text == 'np 10'
        end
      end
    end
    return r
  end
  remove(10)
  rect(10, 12000, 2000, GREEN, 'np 10')
  out.readd_now = read()
  local clock = E.clock() and E.clock or os.clock
  local t0 = clock()
  return E.wait(function()
    if clock() - t0 < 2 then
      return nil
    end
    out.readd_later = read()
    return finish()
  end, 20)
elseif STEP == 'newid' then
  remove(30)
  rect(5, 10000, 0, RED)
elseif STEP == 'over' then
  trigger.action.markupToAll(6, -1, B + 20, p(14000, 4000), p(18000, 4000), p(18000, 8000), p(14000, 8000),
    YELLOW, fill(YELLOW), 1, true, '')
elseif STEP == 'inplace' then
  for _, id in ipairs({ E.store.flicker or 51, 53 }) do
    trigger.action.setMarkupColor(B + id, RED)
    trigger.action.setMarkupColorFill(B + id, fill(YELLOW))
    trigger.action.setMarkupTypeLine(B + id, 2)
    trigger.action.setMarkupFontSize(B + id, 24)
  end
  trigger.action.setMarkupText(B + 53, 'np changed')
elseif STEP == 'geometry' then
  trigger.action.setMarkupPositionEnd(B + (E.store.flicker or 51), p(18000, 20000))
  trigger.action.setMarkupPositionStart(B + 50, p(7000, 12000))
elseif STEP == 'flicker' then
  local clock = E.clock() and E.clock or os.clock
  -- Each second, in one call, remove the line's ID and add a filled quad
  -- (shape 6) at the same place under a fresh ID, as the platform does.
  local t0, n = clock(), 0
  E.store.flicker = E.store.flicker or 51
  return E.wait(function()
    local due = math.floor(clock() - t0)
    if due > n and n < 10 then
      n = n + 1
      remove(E.store.flicker)
      local id = 60 + n
      local a, b = p(11000, 13000), p(15000, 17000)
      local c, d = p(11000, 17000), p(15000, 13000)
      trigger.action.markupToAll(6, -1, B + id, a, c, b, d, BLUE, fill(BLUE), 1, true, '')
      made[B + id] = true
      E.store.flicker = id
    end
    if n >= 10 then
      out.cycles = n
      return finish()
    end
    return nil
  end, 20)
elseif STEP == 'highids' then
  for k, id in ipairs({ 2 ^ 31 - 1, 2 ^ 31, 2 ^ 32, 2 ^ 53 - 1 }) do
    local a, b = corners(-12000, 5000 * (k - 1), 4000)
    trigger.action.markupToAll(3, -1, id, a, b, BLUE, fill(BLUE), 1, true, '')
    made[id] = true
  end
elseif STEP == 'watch' then
  E.store.marks = {}
  local marks = E.store.marks
  E.track.handler({
    onEvent = function(_, ev)
      if ev.id == world.event.S_EVENT_MARK_ADDED or ev.id == world.event.S_EVENT_MARK_CHANGE
          or ev.id == world.event.S_EVENT_MARK_REMOVED then
        local who = ev.initiator
        local name
        if who then
          local ok, n = pcall(function() return who:getPlayerName() or who:getName() end)
          name = ok and n or '<unreadable>'
        end
        marks[#marks + 1] = { event = ev.id, idx = ev.idx, initiator = who ~= nil, initiator_name = name ~= nil,
          coalition = ev.coalition, group = ev.groupID, text = ev.text, time = ev.time }
      end
    end,
  })
elseif STEP == 'read' then
  local function keys(t)
    local k = {}
    for name in pairs(t) do
      k[#k + 1] = tostring(name)
    end
    table.sort(k)
    return table.concat(k, ' ')
  end
  out.marks = E.store.marks
  local panels = {}
  for _, panel in ipairs(world.getMarkPanels()) do
    panels[#panels + 1] = { idx = panel.idx or panel.id, keys = keys(panel), initiator = panel.initiator ~= nil, coalition = panel.coalition,
      group = panel.groupID, text = panel.text, author = panel.author }
  end
  out.panels = panels
elseif STEP == 'clear' then
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
    made[id] = nil
  end
  E.store.round = (E.store.round or 0) + 1
  E.store.flicker = nil
  out.round = E.store.round
else
  error('no step ' .. STEP)
end
return finish()
