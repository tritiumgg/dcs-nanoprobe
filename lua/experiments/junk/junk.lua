-- E-junk: what `world.removeJunk(volume)` does in the mission scripting state.
--
-- Rule, written before the first run (docs/results/2026-11-16-clear-debris.md):
-- the platform builds a clear-debris command when, in one run, (1) a sphere
-- clears every ground wreck inside it, (2) no live unit or live static
-- anywhere is removed by any call, the whole-map ones included, (3) a dead
-- unit outside a local volume is left, and (4) one or more calls clear the
-- wrecks of the whole map. A live object removed fails the rule and stops the
-- step. The rest is recorded: the other shapes, the return value, statics,
-- aircraft, scenery, craters, burning wrecks and their death events, and the
-- cost of each call.
--
-- The snippet lays out clusters of objects 1,500 m apart, 4 to 14 km from the
-- mission's first airbase, kills them with scripted explosions, waits for the
-- burning to end, and clears each cluster with one volume. It reads every
-- object it killed before and after each clear, and counts what
-- `world.searchObjects` finds around the aircraft crashes. Every object it
-- adds is named `np-j-*` and goes at reset; the whole-map calls remove every
-- wreck it made.
-- states: server
-- timeout: 480

local E = ...
local X = E.lib('s15')
local S = X.S
local O = S.origin()
local VT = world.VolumeType
local R = { calls = {}, events = {}, reads = {} }
E.store.junk = R
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

E.observe('removeJunk_type', type(world.removeJunk), { wine = 'function' })
E.observe('volume_types', VT, { info = true })

-- A base where every cluster point is land or road.
local OFFSETS = { { 4000, 4000 }, { -4000, 4000 }, { 4000, -16000 }, { -4000, -16000 }, { 8000, 0 }, { -8000, 0 } }
local SPAN = { 0, 1500, 3000, 4500, 6000, 7500, 9000, 10500, 12000 }
local B
for _, o in ipairs(OFFSETS) do
  local good = true
  for _, dz in ipairs(SPAN) do
    for _, dx in ipairs({ -1500, 0, 3000, 6000 }) do
      local s = land.getSurfaceType({ x = O.x + o[1] + dx, y = O.z + o[2] + dz })
      if s ~= land.SurfaceType.LAND and s ~= land.SurfaceType.ROAD then
        good = false
      end
    end
  end
  if good then
    B = { x = O.x + o[1], z = O.z + o[2] }
    break
  end
end
if not B then
  return 'no land base found'
end
E.observe('base_offset', { x = B.x - O.x, z = B.z - O.z }, { info = true })

local function at(dx, dz)
  return { x = B.x + dx, z = B.z + dz }
end

-- Units: name -> { h, name, cluster, kind }
local units, statics = {}, {}
local function group(name, n, p, spacing, cluster)
  local list = {}
  for i = 1, n do
    list[i] = { name = name .. '-' .. i, x = p.x + (i - 1) * spacing, z = p.z }
  end
  local g, err = X.add_ground(name, 'M-113', list)
  if not g then
    E.note('spawn failed ' .. name .. ': ' .. tostring(err))
    return
  end
  for i = 1, n do
    local u = Unit.getByName(list[i].name)
    units[#units + 1] = { h = u, name = list[i].name, cluster = cluster }
  end
end
local function static(name, p, cluster)
  local s = S.spawn_static(name, p.x - O.x, p.z - O.z, 'FARP Fuel Depot')
  statics[#statics + 1] = { h = s, name = name, cluster = cluster }
end

local C = {
  sphere = at(0, 0),
  box = at(0, 1500),
  segment = at(0, 3000),
  pyramid = at(0, 4500),
  burning = at(0, 6000),
  static = at(0, 7500),
  map = at(0, 9000),
  many = at(0, 10500),
  outside = at(0, 400),
  live = at(-1500, 0),
  crater = at(-1500, 1500),
  heli = at(3000, 3000),
  plane = at(6000, 0),
}
group('np-j-s', 4, C.sphere, 20, 'sphere')
group('np-j-sl', 1, at(30, 30), 0, 'sphere_live')
group('np-j-o', 1, C.outside, 0, 'outside')
group('np-j-x', 4, C.box, 20, 'box')
group('np-j-g', 2, C.segment, 20, 'segment')
group('np-j-y', 2, C.pyramid, 20, 'pyramid')
group('np-j-b', 2, C.burning, 20, 'burning')
group('np-j-w', 4, C.map, 20, 'map')
group('np-j-l', 1, C.live, 0, 'live')
for i = 1, 25 do
  group('np-j-m' .. i, 4, { x = C.many.x, z = C.many.z + (i - 1) * 25 }, 25, 'many')
end
static('np-j-t1', C.static, 'static')
static('np-j-t2', { x = C.static.x + 30, z = C.static.z }, 'static')
static('np-j-tl', { x = C.static.x, z = C.static.z + 300 }, 'static_live')
local heli = X.add_air({ name = 'np-j-h', kind = 'UH-1H', category = Group.Category.HELICOPTER, x = C.heli.x,
  z = C.heli.z, alt = h(C.heli.x, C.heli.z) + 60, speed = 20, fuel = 300, task = 'Transport' })
local plane = X.add_air({ name = 'np-j-f', x = C.plane.x, z = C.plane.z, alt = h(C.plane.x, C.plane.z) + 300,
  speed = 120, heading = 0, task = 'Nothing' })
E.observe('spawned', { units = #units, statics = #statics, heli = heli ~= nil, plane = plane ~= nil }, { info = true })

-- A scenery object near the airbase.
local scenery
pcall(world.searchObjects, Object.Category.SCENERY,
  { id = VT.SPHERE, params = { point = pt(O.x, O.z), radius = 4000 } }, function(o)
    local ok, life = pcall(o.getLife, o)
    if ok and life and life > 0 then
      scenery = o
      return false
    end
    return true
  end)

local function rv(ok, v)
  if not ok then
    return 'raise'
  end
  return tostring(v)
end

local function read_unit(u)
  return 'exist=' .. rv(pcall(Object.isExist, u.h)) .. ' dead=' .. rv(pcall(Unit.isDead, u.h)) .. ' life='
    .. rv(pcall(Unit.getLife, u.h)) .. ' point=' .. (pcall(Object.getPoint, u.h) and 'yes' or 'raise') .. ' named='
    .. tostring(Unit.getByName(u.name) ~= nil)
end
local function read_static(s)
  return 'exist=' .. rv(pcall(Object.isExist, s.h)) .. ' life=' .. rv(pcall(StaticObject.getLife, s.h))
    .. ' point=' .. (pcall(Object.getPoint, s.h) and 'yes' or 'raise') .. ' named='
    .. tostring(StaticObject.getByName(s.name) ~= nil)
end

-- Per cluster, how many objects read each way.
local function snapshot(label)
  local out = {}
  local function add(cluster, text)
    out[cluster] = out[cluster] or {}
    out[cluster][text] = (out[cluster][text] or 0) + 1
  end
  for _, u in ipairs(units) do
    add(u.cluster, read_unit(u))
  end
  for _, s in ipairs(statics) do
    add(s.cluster, read_static(s))
  end
  if scenery then
    add('scenery', 'exist=' .. rv(pcall(scenery.isExist, scenery)) .. ' life=' .. rv(pcall(scenery.getLife, scenery)))
  end
  add('crater', 'height=' .. string.format('%.2f', h(C.crater.x, C.crater.z)))
  R.reads[label] = out
  E.observe('read_' .. label, out, { info = true })
end

-- What world.searchObjects finds in a sphere, by category and type.
local function census(label, p, r)
  local out = {}
  local ok, err = pcall(world.searchObjects,
    { Object.Category.UNIT, Object.Category.WEAPON, Object.Category.STATIC, Object.Category.BASE,
      Object.Category.SCENERY, Object.Category.CARGO },
    { id = VT.SPHERE, params = { point = pt(p.x, p.z), radius = r } }, function(o)
      local okc, cat = pcall(o.getCategory, o)
      local okt, kind = pcall(o.getTypeName, o)
      local key = rv(okc, cat) .. ':' .. rv(okt, kind)
      out[key] = (out[key] or 0) + 1
      return true
    end)
  if not ok then
    out.error = tostring(err)
  end
  E.observe('census_' .. label, out, { info = true })
end

local function junk(label, vol)
  local c0 = E.clock()
  local ok, n = pcall(world.removeJunk, vol)
  local c1 = E.clock()
  local rec = { label = label, ok = ok, type = type(n), value = n == nil and 'nil' or (ok and n or tostring(n)), t = t(),
    us = (c0 and c1) and math.floor((c1 - c0) * 1e6) or 'no clock' }
  R.calls[#R.calls + 1] = rec
  E.observe('call_' .. label, rec, { info = true })
  return rec
end
local function sphere(p, r, dy)
  return { id = VT.SPHERE, params = { point = pt(p.x, p.z, dy), radius = r } }
end
local function box(p, dx0, dx1, dz0, dz1)
  local y = h(p.x, p.z)
  return { id = VT.BOX, params = { min = { x = p.x + dx0, y = y - 100, z = p.z + dz0 },
    max = { x = p.x + dx1, y = y + 500, z = p.z + dz1 } } }
end

E.track.handler({ onEvent = function(self, e)
  if e.id == world.event.S_EVENT_DEAD or e.id == world.event.S_EVENT_CRASH or e.id == world.event.S_EVENT_KILL
    or e.id == world.event.S_EVENT_UNIT_LOST then
    local ok, name = pcall(function() return e.initiator:getName() end)
    if ok and type(name) == 'string' and name:sub(1, 4) == 'np-j' then
      R.events[#R.events + 1] = string.format('%.1f %d %s', t(), e.id, name)
    end
  end
end })

-- Cost with nothing dead yet.
junk('empty_small', sphere(C.sphere, 50))
junk('empty_map', sphere({ x = 0, z = 0 }, 2e6, -h(0, 0)))

local function kill_unit(name, power, offset)
  local u = Unit.getByName(name)
  if u then
    local p = u:getPoint()
    trigger.action.explosion({ x = p.x, y = p.y, z = p.z - (offset or 0) }, power)
  end
end

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

local plane_point, heli_point
return E.wait(function()
  once('kill', 3, function()
    snapshot('alive')
    census('plane_alive', C.plane, 3000)
    for _, u in ipairs(units) do
      if u.cluster ~= 'burning' and u.cluster ~= 'live' and u.cluster ~= 'sphere_live' then
        kill_unit(u.name, 30, 6)
      end
    end
    for _, s in ipairs(statics) do
      if s.cluster == 'static' then
        trigger.action.explosion(s.h:getPoint(), 1000)
      end
    end
    local hu = Unit.getByName('np-j-h-1')
    if hu then
      heli_point = hu:getPoint()
      trigger.action.explosion(heli_point, 1000)
    end
    local fu = Unit.getByName('np-j-f-1')
    if fu then
      plane_point = fu:getPoint()
      trigger.action.explosion(plane_point, 1000)
    end
    if scenery then
      local p = scenery:getPoint()
      E.observe('scenery', { offset_x = p.x - O.x, offset_z = p.z - O.z, life = scenery:getLife() }, { info = true })
      trigger.action.explosion(p, 500)
    end
    trigger.action.explosion(pt(C.crater.x, C.crater.z), 500)
  end)
  once('kill_burning', 60, function()
    for _, u in ipairs(units) do
      if u.cluster == 'burning' then
        kill_unit(u.name, 30, 6)
      end
    end
  end)
  once('clear_burning', 63, function()
    snapshot('burning_before')
    junk('burning_sphere', sphere(C.burning, 150))
  end)
  once('read_burning', 65, function()
    snapshot('burning_after')
  end)
  once('before', 95, function()
    snapshot('before')
    if heli_point then
      census('heli_before', heli_point, 600)
    end
    if plane_point then
      census('plane_before', { x = plane_point.x + 1000, z = plane_point.z }, 3000)
    end
    census('static_before', C.static, 100)
  end)
  once('clear', 100, function()
    local m = units[#units]
    for _, u in ipairs(units) do
      if u.cluster == 'many' then
        m = u
        break
      end
    end
    local mp = m.h:getPoint()
    junk('many_one', sphere(mp, 5))
    junk('sphere', sphere(C.sphere, 150))
    junk('box', box(C.box, -50, 150, -50, 50))
    local y = h(C.segment.x, C.segment.z) + 1
    junk('segment', { id = VT.SEGMENT, params = { from = { x = C.segment.x - 100, y = y, z = C.segment.z },
      to = { x = C.segment.x + 120, y = y, z = C.segment.z } } })
    local py = h(C.pyramid.x, C.pyramid.z)
    junk('pyramid', { id = VT.PYRAMID, params = { pos = { p = { x = C.pyramid.x + 10, y = py + 400, z = C.pyramid.z },
      x = { x = 0, y = -1, z = 0 }, y = { x = 1, y = 0, z = 0 }, z = { x = 0, y = 0, z = 1 } }, length = 600,
      halfAngleHor = 0.6, halfAngleVer = 0.6 } })
    junk('static', sphere({ x = C.static.x + 15, z = C.static.z }, 100))
    if heli_point then
      junk('heli', sphere(heli_point, 600))
    end
    if plane_point then
      junk('plane', sphere({ x = plane_point.x + 1000, z = plane_point.z }, 3000))
    end
    if scenery then
      junk('scenery', sphere(scenery:getPoint(), 60))
    end
    junk('crater', sphere(C.crater, 50))
  end)
  once('after', 103, function()
    snapshot('after')
    if heli_point then
      census('heli_after', heli_point, 600)
    end
    if plane_point then
      census('plane_after', { x = plane_point.x + 1000, z = plane_point.z }, 3000)
    end
    census('static_after', C.static, 100)
  end)
  once('map', 110, function()
    junk('map_sphere', sphere({ x = 0, z = 0 }, 2e6, -h(0, 0)))
  end)
  once('after_map', 113, function()
    snapshot('after_map')
  end)
  once('map_again', 116, function()
    junk('map_sphere_again', sphere({ x = 0, z = 0 }, 2e6, -h(0, 0)))
    junk('map_box', { id = VT.BOX, params = { min = { x = -1e6, y = -1e4, z = -1e6 }, max = { x = 1e6, y = 1e5, z = 1e6 } } })
    junk('map_sphere_1e7', sphere({ x = 0, z = 0 }, 1e7, -h(0, 0)))
  end)
  if t() >= 160 then
    snapshot('end')
    E.observe('events', R.events, { info = true })
    return 'done'
  end
end, 470, 'running')
