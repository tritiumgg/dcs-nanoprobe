-- Drawing layers: the order DCS keeps markup in, read through world.getMarkPanels,
-- and what an update, a re-add and an add over a held ID do to it. Also the
-- events each call emits, and the panels a mission already holds (player marks).
-- Every drawing it makes is removed before it returns, and again at reset.
-- states: server
-- timeout: 60

local E = ...

-- IDs from 900,000,000, below the platform pool (2^30 to 2^31 - 1).
local B = 900000000
local made = {}
E.track.defer('drawing-layers order', function()
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
  end
end)

local function p(x, z)
  return { x = x, y = 0, z = z }
end
local RED, FILL = { 1, 0, 0, 1 }, { 1, 0, 0, 0.3 }
local BLUE, BFILL = { 0, 0, 1, 1 }, { 0, 0, 1, 0.3 }

-- Each shape with its arguments as DCS takes them, at offset k. 'rect' is shape 3
-- given two points, which a client draws as a line (LIVE, run 3 part B).
local function shape(kind, k)
  local x = 1000 * k
  if kind == 'line' then
    return 1, { p(x, 0), p(x + 800, 800), RED, 1, true, '' }
  elseif kind == 'rect' then
    return 3, { p(x, 0), p(x + 800, 800), RED, FILL, 1, true, '' }
  elseif kind == 'text' then
    return 5, { p(x, 0), RED, FILL, 14, true, 'np ' .. k }
  elseif kind == 'quad' then
    return 6, { p(x, 0), p(x + 800, 0), p(x + 800, 800), p(x, 800), RED, FILL, 1, true, '' }
  elseif kind == 'free' then
    return 7, { p(x, 0), p(x + 800, 0), p(x + 800, 800), p(x + 400, 1200), p(x, 800), RED, FILL, 1, true, '' }
  end
  error('no shape ' .. kind)
end

local function add(id, kind, k)
  local s, args = shape(kind, k)
  local ok, err = pcall(trigger.action.markupToAll, s, -1, id, unpack(args))
  made[id] = true
  return ok and 'ok' or tostring(err)
end

local function remove(id)
  local ok, err = pcall(trigger.action.removeMark, id)
  return ok and 'ok' or tostring(err)
end

-- Our IDs in the order getMarkPanels lists them, and each one's panel text and pos.
local function ours()
  local order, seen = {}, {}
  local ok, panels = pcall(world.getMarkPanels)
  if not ok or type(panels) ~= 'table' then
    return { error = tostring(panels) }
  end
  for i, panel in ipairs(panels) do
    local id = panel.idx or panel.id
    if type(id) == 'number' and id >= B and id < B + 1000 then
      order[#order + 1] = id - B
      seen[tostring(id - B)] = { at = i, text = panel.text, pos = panel.pos }
    end
  end
  return { order = order, panels = seen }
end

-- Events by kind, for our IDs and for any other.
local events = {}
E.track.handler({
  onEvent = function(_, ev)
    local name
    if ev.id == world.event.S_EVENT_MARK_ADDED then
      name = 'added'
    elseif ev.id == world.event.S_EVENT_MARK_CHANGE then
      name = 'change'
    elseif ev.id == world.event.S_EVENT_MARK_REMOVED then
      name = 'removed'
    end
    if name then
      local id = ev.idx
      local key = (type(id) == 'number' and id >= B and id < B + 1000) and tostring(id - B) or 'other'
      events[#events + 1] = { kind = name, id = key, initiator = ev.initiator ~= nil, text = ev.text }
    end
  end,
})

local out = { steps = {} }
local function step(name, results)
  out.steps[#out.steps + 1] = { name = name, results = results, after = ours() }
end

-- What the mission holds before: count, the ID range and one panel's keys.
do
  local ok, panels = pcall(world.getMarkPanels)
  local before = { ok = ok, count = ok and type(panels) == 'table' and #panels or nil, ids = {} }
  if ok and type(panels) == 'table' then
    for i, panel in ipairs(panels) do
      before.ids[#before.ids + 1] = panel.idx or panel.id
      if i == 1 then
        local keys = {}
        for k in pairs(panel) do
          keys[#keys + 1] = tostring(k)
        end
        table.sort(keys)
        before.keys = keys
        before.first = panel
      end
    end
  end
  out.before = before
end

-- 1. Mixed shapes, IDs out of order: 3 shape-3 line, 1 line, 2 text.
step('mixed 3,1,2', { add(B + 3, 'rect', 3), add(B + 1, 'line', 1), add(B + 2, 'text', 2) })
-- 2. One shape (shape 3, a line), IDs out of order: 13, 11, 12.
step('rects 13,11,12', { add(B + 13, 'rect', 13), add(B + 11, 'rect', 11), add(B + 12, 'rect', 12) })
-- 3. Remove 1 and add it again under the same ID.
step('re-add 1', { remove(B + 1), add(B + 1, 'line', 1) })
-- 4. Remove 2 and add it under a lower, new ID 0.
step('move 2 to 0', { remove(B + 2), add(B + 0, 'text', 2) })
-- 5. Add over a held ID, with no remove first: 11 as a quad.
step('add over 11', { add(B + 11, 'quad', 11) })
-- 6. Updates in place.
do
  local r = {}
  r[1] = tostring(select(2, pcall(trigger.action.setMarkupText, B + 0, 'np changed')) or 'ok')
  r[2] = tostring(select(2, pcall(trigger.action.setMarkupColor, B + 13, BLUE)) or 'ok')
  r[3] = tostring(select(2, pcall(trigger.action.setMarkupColorFill, B + 13, BFILL)) or 'ok')
  r[4] = tostring(select(2, pcall(trigger.action.setMarkupTypeLine, B + 3, 2)) or 'ok')
  r[5] = tostring(select(2, pcall(trigger.action.setMarkupFontSize, B + 0, 20)) or 'ok')
  step('set in place 0,13,3', r)
end
-- 7. A freeform's start point moved: does the panel's pos follow?
step('free 20', { add(B + 20, 'free', 20) })
do
  local r = {
    tostring(select(2, pcall(trigger.action.setMarkupPositionStart, B + 20, p(20000, 5000))) or 'ok'),
    tostring(select(2, pcall(trigger.action.setMarkupPositionEnd, B + 3, p(9000, 9000))) or 'ok'),
  }
  step('move start 20, end 3', r)
end

-- Real time: the snippet never resumes a paused server.
local clock = E.clock() and E.clock or os.clock
local t0, removed_at = clock(), nil
return E.wait(function()
  local now = clock()
  if not removed_at then
    if now - t0 < 2 then
      return nil
    end
    for id in pairs(made) do
      remove(id)
    end
    made = {}
    out.removed = ours()
    removed_at = now
    return nil
  end
  if now - removed_at < 1 then
    return nil
  end
  out.events = events
  return out
end, 20)
