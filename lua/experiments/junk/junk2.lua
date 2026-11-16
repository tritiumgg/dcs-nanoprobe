-- E-junk2: can one dead object's wreck be removed on its own?
--
-- (a) Object.destroy on a dead object's handle, 1 s and 300 s after its
-- death, for ground units, statics and helicopters. A server shows nothing,
-- so the snippet reads what it can: the handle's reads, getByName,
-- world.searchObjects, the death events, and the count a world.removeJunk
-- sphere on the spot returns afterward, against a control that was not
-- destroyed. A smaller count than the control's means the destroy took the
-- wreck out of DCS's junk.
-- (b) world.removeJunk spheres of 1, 3 and 8 m on one wreck, with dead
-- neighbors 10, 30 and 70 m from it: which neighbors go too.
--
-- Rule (docs/results/2026-11-16-clear-debris.md, "Per wreck"): per-wreck
-- removal by destroy works when the destroyed objects' spheres count less
-- than their controls' at 300 s; by a small sphere when one radius removes
-- its wreck and no neighbor.
-- states: server
-- timeout: 480

local E = ...
local X = E.lib('s15')
local S = X.S
local O = S.origin()
local VT = world.VolumeType
local R = { events = {} }
E.store.junk2 = R
local t0 = timer.getTime()
local function t()
  return timer.getTime() - t0
end
local function h(x, z)
  return land.getHeight({ x = x, y = z })
end
local function pt(x, z, dy)
  return { x = x, y = h(x, z) + (dy or 0), z = z }
end

-- The base E-junk found on land: 4 km north and 4 km east of the airbase.
local B = { x = O.x + 4000, z = O.z + 4000 }
local function at(dx, dz)
  return { x = B.x + dx, z = B.z + dz }
end

local subjects = {}
local function ground(name, p)
  local g = X.add_ground(name, 'M-113', { { name = name .. '-1', x = p.x, z = p.z } })
  local u = g and Unit.getByName(name .. '-1')
  subjects[name] = { kind = 'ground', h = u, name = name .. '-1', p = p }
end
local function static(name, p)
  local s = S.spawn_static(name, p.x - O.x, p.z - O.z, 'FARP Fuel Depot')
  subjects[name] = { kind = 'static', h = s, name = name, p = p }
end
local function heli(name, p)
  local g = X.add_air({ name = name, kind = 'UH-1H', category = Group.Category.HELICOPTER, x = p.x, z = p.z,
    alt = h(p.x, p.z) + 60, speed = 20, fuel = 300, task = 'Transport' })
  subjects[name] = { kind = 'heli', h = g and Unit.getByName(name .. '-1'), name = name .. '-1', p = p }
end

-- (a) Destroyed at 1 s, at 300 s, and controls, 300 m apart (statics and
-- helicopters 2 km apart, so a 1,000 power kill touches nothing else).
ground('np-j2-g1a', at(0, 0))
ground('np-j2-g1b', at(0, 300))
ground('np-j2-g5a', at(0, 600))
ground('np-j2-g5b', at(0, 900))
ground('np-j2-gka', at(0, 1200))
ground('np-j2-gkb', at(0, 1500))
static('np-j2-s1', at(2000, 0))
static('np-j2-s5', at(2000, 2000))
static('np-j2-sk', at(2000, 4000))
heli('np-j2-h1', at(4000, 0))
heli('np-j2-h5', at(4000, 2000))
heli('np-j2-hk', at(4000, 4000))
-- (b) Rows: a target and neighbors 10, 30 and 70 m from it.
local ROWS = { r1 = 1, r3 = 3, r8 = 8 }
local row_at = { r1 = at(-2000, 0), r3 = at(-2000, 500), r8 = at(-2000, 1000) }
for row, _ in pairs(ROWS) do
  local p = row_at[row]
  for i, d in ipairs({ 0, 10, 30, 70 }) do
    ground('np-j2-' .. row .. '-' .. i, { x = p.x + d, z = p.z })
  end
end

local function rv(ok, v)
  if not ok then
    return 'raise'
  end
  return tostring(v)
end
local function read(s)
  if s.kind == 'static' then
    return 'exist=' .. rv(pcall(Object.isExist, s.h)) .. ' life=' .. rv(pcall(StaticObject.getLife, s.h)) .. ' named='
      .. tostring(StaticObject.getByName(s.name) ~= nil)
  end
  return 'exist=' .. rv(pcall(Object.isExist, s.h)) .. ' dead=' .. rv(pcall(Unit.isDead, s.h)) .. ' life='
    .. rv(pcall(Unit.getLife, s.h)) .. ' point=' .. (pcall(Object.getPoint, s.h) and 'yes' or 'raise')
end
local function snapshot(label)
  local out = {}
  for name, s in pairs(subjects) do
    out[name] = read(s)
  end
  E.observe('read_' .. label, out, { info = true })
end
local function census(label, p, r)
  local out = {}
  pcall(world.searchObjects, { Object.Category.UNIT, Object.Category.STATIC, Object.Category.CARGO },
    { id = VT.SPHERE, params = { point = pt(p.x, p.z), radius = r } }, function(o)
      local okt, kind = pcall(o.getTypeName, o)
      local key = rv(okt, kind)
      out[key] = (out[key] or 0) + 1
      return true
    end)
  return out
end
local function junk(p, r)
  local c0 = E.clock()
  local ok, n = pcall(world.removeJunk, { id = VT.SPHERE, params = { point = pt(p.x, p.z), radius = r } })
  local c1 = E.clock()
  return { ok = ok, value = ok and n or tostring(n), us = (c0 and c1) and math.floor((c1 - c0) * 1e6) or 'no clock' }
end
local function destroy(name)
  local s = subjects[name]
  local ok, err = pcall(Object.destroy, s.h)
  return ok and 'ok' or ('raise: ' .. tostring(err))
end

E.track.handler({ onEvent = function(self, e)
  if e.id == world.event.S_EVENT_DEAD or e.id == world.event.S_EVENT_CRASH or e.id == world.event.S_EVENT_UNIT_LOST
    or e.id == world.event.S_EVENT_KILL then
    local ok, name = pcall(function() return e.initiator:getName() end)
    if ok and type(name) == 'string' and name:sub(1, 5) == 'np-j2' then
      R.events[#R.events + 1] = string.format('%.1f %d %s', t(), e.id, name)
    end
  end
end })

local done = {}
local function once(name, at_s, fn)
  if not done[name] and t() >= at_s then
    done[name] = true
    local ok, err = pcall(fn)
    if not ok then
      E.note(name .. ' failed: ' .. tostring(err))
    end
  end
end

local kill_point = {}
return E.wait(function()
  once('kill', 3, function()
    snapshot('alive')
    for name, s in pairs(subjects) do
      local ok, p = pcall(Object.getPoint, s.h)
      if ok then
        kill_point[name] = p
        if s.kind == 'ground' then
          trigger.action.explosion({ x = p.x, y = p.y, z = p.z - 6 }, 30)
        else
          trigger.action.explosion(p, 1000)
        end
      end
    end
  end)
  once('destroy_1s', 4, function()
    snapshot('dead_1s')
    E.observe('destroy_1s', { g1a = destroy('np-j2-g1a'), g1b = destroy('np-j2-g1b'), s1 = destroy('np-j2-s1'),
      h1 = destroy('np-j2-h1') }, { info = true })
  end)
  once('read_1s', 6, function()
    snapshot('after_destroy_1s')
  end)
  once('destroy_300s', 303, function()
    snapshot('dead_300s')
    local c = {}
    for _, n in ipairs({ 'np-j2-s1', 'np-j2-s5', 'np-j2-sk' }) do
      c[n] = census(n, subjects[n].p, 50)
    end
    E.observe('census_statics_before', c, { info = true })
    E.observe('destroy_300s', { g5a = destroy('np-j2-g5a'), g5b = destroy('np-j2-g5b'), s5 = destroy('np-j2-s5'),
      h5 = destroy('np-j2-h5') }, { info = true })
  end)
  once('clear', 306, function()
    snapshot('after_destroy_300s')
    local c = {}
    for _, n in ipairs({ 'np-j2-s1', 'np-j2-s5', 'np-j2-sk' }) do
      c[n] = census(n, subjects[n].p, 50)
    end
    E.observe('census_statics_after', c, { info = true })
    -- (a) the same sphere on each subject: 30 m for ground and statics,
    -- 600 m round a helicopter's kill point.
    local counts = {}
    for name, s in pairs(subjects) do
      if not name:find('-r%d') then
        local p = kill_point[name] or s.p
        counts[name] = junk(p, s.kind == 'heli' and 600 or 30)
      end
    end
    E.observe('sphere_counts', counts, { info = true })
    -- (b) the small spheres on each row's target.
    local small = {}
    for row, r in pairs(ROWS) do
      local p = kill_point['np-j2-' .. row .. '-1'] or row_at[row]
      small[row] = junk(p, r)
    end
    E.observe('small_counts', small, { info = true })
  end)
  once('after', 309, function()
    snapshot('after_clear')
  end)
  if t() >= 312 then
    E.observe('events', R.events, { info = true })
    -- Clear what is left of the snippet's junk round the base.
    E.observe('cleanup', junk(at(1000, 2000), 8000), { info = true })
    return 'done'
  end
end, 470, 'running')
