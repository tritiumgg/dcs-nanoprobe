-- E21b: may platform names use a base-64 alphabet? Rule: every name below is
-- accepted, found by getByName, and read back byte for byte by getName, in
-- the lookup, in a coalition.getGroups / getStaticObjects listing and in an
-- event initiator; names differing only in case stay distinct.
--
-- a. case: groups np.g.aB and np.g.Ab with units np.u.aB and np.u.Ab, statics
--    np.s.aB and np.s.Ab, and unit xY against static Xy (one namespace).
-- b. alphabet: a unit, a static and a group whose names hold every one of
--    A-Z a-z 0-9 - _ + /, and one of each starting with `~`.
-- c. leading characters: the names .k3F19, #k3F7 and !k3F2A as a unit and as
--    a group each, a static with each prefix (the name plus an `s`), and
--    unit .k3F19 beside static !k3F19 (shared namespace: do both survive).
--
-- Each case records whether the add raised, getByName, getName equality and
-- getID (cases a compare the IDs of the pairs). A world event handler,
-- added and removed here, collects the names of birth and death initiators.
-- Everything is removed at the end. The mission loads paused, so the wait
-- runs with 'running': the hook resumes the simulation and model time
-- advances.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local o = S.origin()

local ALPHA = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_+/'

local cases = {}
local row = 0
local function pos()
  row = row + 1
  return o.x + 6000 + row * 40, o.z + 3000 + row * 40
end

local function add_group(name, unit_name)
  local x, z = pos()
  local ok, err = pcall(S.ground, name, 'M-113', { { name = unit_name, x = x, z = z } })
  cases[#cases + 1] = { kind = 'group', name = name, added = ok, err = (not ok) and tostring(err) or nil }
  cases[#cases + 1] = { kind = 'unit', name = unit_name, added = ok, err = (not ok) and tostring(err) or nil }
end

local function add_static(name)
  local x, z = pos()
  local ok, err = pcall(S.spawn_static, name, x - o.x, z - o.z)
  cases[#cases + 1] = { kind = 'static', name = name, added = ok, err = (not ok) and tostring(err) or nil }
end

-- a. case
add_group('np.g.aB', 'np.u.aB')
add_group('np.g.Ab', 'np.u.Ab')
add_static('np.s.aB')
add_static('np.s.Ab')
add_group('np.g.xYhost', 'xY')
add_static('Xy')
-- b. alphabet
add_group('G' .. ALPHA, 'U' .. ALPHA)
add_static('S' .. ALPHA)
add_group('~G', '~U')
add_static('~S')
-- c. leading characters
for _, name in ipairs({ '.k3F19', '#k3F7', '!k3F2A' }) do
  add_group(name, 'np.h.' .. name)
  add_static(name .. 's')
end
-- the units with the exact names, hosted by their own groups
add_group('np.hu..k3F19', '.k3F19')
add_group('np.hu.#k3F7', '#k3F7')
add_group('np.hu.!k3F2A', '!k3F2A')
add_static('!k3F19')

local lookup = { Group = Group.getByName, unit = Unit.getByName, static = StaticObject.getByName }
lookup.group = Group.getByName

local function id_of(obj)
  local ok, id = pcall(obj.getID, obj)
  return ok and id or nil
end

-- Names listed by the coalition calls, over both sides.
local listed = { group = {}, unit = {}, static = {} }
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE, coalition.side.NEUTRAL }) do
  local okg, groups = pcall(coalition.getGroups, side)
  for _, g in ipairs(okg and groups or {}) do
    listed.group[g:getName()] = true
    for _, u in ipairs(g:getUnits() or {}) do
      listed.unit[u:getName()] = true
    end
  end
  local oks, statics = pcall(coalition.getStaticObjects, side)
  for _, s in ipairs(oks and statics or {}) do
    listed.static[s:getName()] = true
  end
end

local ids, rows = {}, {}
local failures = 0
for _, c in ipairs(cases) do
  local obj = lookup[c.kind](c.name)
  local row_out = {
    kind = c.kind,
    name = c.name,
    added = c.added,
    err = c.err,
    found = obj ~= nil,
    name_back = obj and obj:getName() == c.name or false,
    in_listing = listed[c.kind][c.name] == true,
    id = obj and id_of(obj) or nil,
  }
  ids[c.kind .. ':' .. c.name] = row_out.id
  if not (row_out.added and row_out.found and row_out.name_back and row_out.in_listing) then
    failures = failures + 1
  end
  rows[#rows + 1] = row_out
end
E.observe('cases', rows, { info = true })

local function distinct(a, b)
  return ids[a] ~= nil and ids[b] ~= nil and ids[a] ~= ids[b]
end
E.observe('case_pairs', {
  groups_aB_Ab_distinct = distinct('group:np.g.aB', 'group:np.g.Ab'),
  units_aB_Ab_distinct = distinct('unit:np.u.aB', 'unit:np.u.Ab'),
  statics_aB_Ab_distinct = distinct('static:np.s.aB', 'static:np.s.Ab'),
  unit_xY_static_Xy_distinct = distinct('unit:xY', 'static:Xy'),
  unit_dot_static_bang_both_found = ids['unit:.k3F19'] ~= nil and ids['static:!k3F19'] ~= nil,
}, { info = true })
E.observe('failures', failures, { info = true })

-- Events: a handler collects initiator names for births already past (the
-- adds above ran before it, so spawn one more of each kind), and for deaths.
local seen = {}
local handler = { onEvent = function(_, event)
  local ok, name = pcall(function() return event.initiator and event.initiator:getName() end)
  seen[#seen + 1] = { id = event.id, name = ok and name or ('ERR ' .. tostring(name)) }
end }
world.addEventHandler(handler)
local ev_names = { unit = '.ev3F19', group = '#ev3F7', static = '!ev3F2A' }
add_group(ev_names.group, ev_names.unit)
add_static(ev_names.static)
pcall(S.kill, '.k3F19', 200)
pcall(S.kill, 'xY', 200)
pcall(S.kill, ev_names.unit, 200)

local started, finished = timer.getTime(), false
local function finish()
  pcall(world.removeEventHandler, handler)
  local wanted = {}
  for _, n in pairs(ev_names) do wanted[n] = true end
  wanted['.k3F19'] = true
  wanted['xY'] = true
  local matched, other = {}, 0
  for _, s in ipairs(seen) do
    if wanted[s.name] then
      matched[#matched + 1] = s
    else
      other = other + 1
    end
  end
  E.observe('events', { matched = matched, others = other, total = #seen, event_ids = {
    BIRTH = world.event.S_EVENT_BIRTH, DEAD = world.event.S_EVENT_DEAD,
    UNIT_LOST = world.event.S_EVENT_UNIT_LOST, KILL = world.event.S_EVENT_KILL } }, { info = true })
  local g, s = S.clear()
  E.observe('cleared', { groups = g, statics = s }, { info = true })
  local left = {}
  for _, c in ipairs(cases) do
    if lookup[c.kind](c.name) then left[#left + 1] = c.kind .. ':' .. c.name end
  end
  E.observe('leftovers', left, { info = true })
end

return E.wait(function()
  if timer.getTime() - started < 6 then
    return nil
  end
  if not finished then
    finished = true
    finish()
  end
  return 'done'
end, 60, 'running')
