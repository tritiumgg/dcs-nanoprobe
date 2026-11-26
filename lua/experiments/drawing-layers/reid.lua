-- Drawing layers: does an add under an ID that was removed add a panel again? A
-- drawing (markupToAll, shape 3 with two points, a line on a client) and a mark (markToAll) are added, removed and
-- added again under the same ID, in the same call and after real time, and
-- world.getMarkPanels is read for the ID after each action. Also a remove, then an
-- add under a fresh ID, and the events each ID emitted. Every ID it used is removed
-- before it returns, and again at reset.
--   a  line under A: read; remove; read; re-add in the same call; read; remove again
--   b  line under B: remove; wait 2 s; re-add; read; wait 2 s more; read
--   c  line under C: remove; add under a fresh ID D; read C and D
--   d  mark under M: remove; re-add in the same call; read; read again at 2 s and 4 s
--   e  events per ID: S_EVENT_MARK_ADDED and S_EVENT_MARK_REMOVED counts
-- The waits are on real time (E.clock), so a paused server stays paused.
-- states: server
-- timeout: 60
-- Run with: run.sh <server> reid
-- Evidence: docs/results/2026-11-25-drawing-layers.md, run 3 (LIVE).

local E = ...

-- IDs from 900,000,000, below the platform pool (2^30 to 2^31 - 1), and clear of
-- the other snippets' offsets (0-20 for order, 1000 and up for client).
local BASE = 900000000
local ID = { A = BASE + 600, B = BASE + 601, C = BASE + 602, D = BASE + 603, M = BASE + 604 }
local NAME = {}
for name, id in pairs(ID) do
  NAME[id] = name
end

local made = {}
E.track.defer('drawing-layers reid', function()
  for id in pairs(made) do
    pcall(trigger.action.removeMark, id)
  end
end)

local function p(x, z)
  return { x = x, y = 0, z = z }
end

local function drawing(name)
  local x = 1000 * (ID[name] - BASE - 600)
  local ok, err = pcall(trigger.action.markupToAll, 3, -1, ID[name], p(x, 0), p(x + 800, 800), { 1, 0, 0, 1 }, { 1, 0, 0, 0.3 }, 1, true, '')
  made[ID[name]] = true
  return ok and 'ok' or tostring(err)
end

local function mark(name)
  local ok, err = pcall(trigger.action.markToAll, ID[name], 'np reid ' .. name, p(5000, 0), true)
  made[ID[name]] = true
  return ok and 'ok' or tostring(err)
end

local function remove(name)
  local ok, err = pcall(trigger.action.removeMark, ID[name])
  return ok and 'ok' or tostring(err)
end

-- The panel under an ID: whether one exists, its text, and how many panels hold it.
local function read(name)
  local ok, panels = pcall(world.getMarkPanels)
  if not ok or type(panels) ~= 'table' then
    return { error = tostring(panels) }
  end
  local row = { exists = false, count = 0 }
  for _, panel in ipairs(panels) do
    if (panel.idx or panel.id) == ID[name] then
      row.exists = true
      row.count = row.count + 1
      row.text = panel.text
    end
  end
  return row
end

local events = {}
for _, name in pairs(NAME) do
  events[name] = { added = 0, removed = 0, change = 0 }
end
E.track.handler({
  onEvent = function(_, ev)
    local kind
    if ev.id == world.event.S_EVENT_MARK_ADDED then
      kind = 'added'
    elseif ev.id == world.event.S_EVENT_MARK_CHANGE then
      kind = 'change'
    elseif ev.id == world.event.S_EVENT_MARK_REMOVED then
      kind = 'removed'
    end
    local name = kind and NAME[ev.idx]
    if name then
      events[name][kind] = events[name][kind] + 1
    end
  end,
})

local out = { a = {}, b = {}, c = {}, d = {}, mark_later = {} }
local function act(case, label, call, name)
  out[case][#out[case] + 1] = { action = label, result = call and call(name) or nil, panel = read(name) }
end

-- a. Same call.
act('a', 'add A', drawing, 'A')
act('a', 'remove A', remove, 'A')
act('a', 're-add A', drawing, 'A')
act('a', 'second remove A', remove, 'A')

-- b, first half: add, remove, and wait.
act('b', 'add B', drawing, 'B')
act('b', 'remove B', remove, 'B')

-- c. A fresh ID.
act('c', 'add C', drawing, 'C')
act('c', 'remove C', remove, 'C')
act('c', 'add D', drawing, 'D')
out.c[#out.c].panel_C = read('C')

-- d. A mark, same call.
act('d', 'mark M', mark, 'M')
act('d', 'remove M', remove, 'M')
act('d', 're-add mark M', mark, 'M')

-- Real time: the snippet never resumes a paused server.
local clock = E.clock() and E.clock or os.clock
local t0 = clock()
local phase = 1
local removed_at
return E.wait(function()
  local now = clock() - t0
  if phase == 1 then
    if now < 2 then
      return nil
    end
    act('b', 're-add B after 2 s', drawing, 'B')
    out.mark_later[#out.mark_later + 1] = { at = '2 s', M = read('M'), A = read('A'), C = read('C'), D = read('D') }
    phase = 2
  elseif phase == 2 then
    if now < 4 then
      return nil
    end
    act('b', 'read B after 4 s', nil, 'B')
    out.mark_later[#out.mark_later + 1] = { at = '4 s', M = read('M'), A = read('A'), C = read('C'), D = read('D') }
    for name in pairs(ID) do
      remove(name)
    end
    made = {}
    out.removed = {}
    for name in pairs(ID) do
      out.removed[name] = read(name)
    end
    removed_at = clock()
    phase = 3
  elseif phase == 3 then
    if clock() - removed_at < 1 then
      return nil
    end
    out.final = {}
    for name in pairs(ID) do
      out.final[name] = read(name)
    end
    out.events = events
    return out
  end
  return nil
end, 20)
