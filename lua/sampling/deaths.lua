-- How the agent's object table classifies a row that DCS no longer has
-- (the S10 confirming run, DECISIONS 2026-11-14 00:38 item 7 and 01:27). Units
-- and statics cease to exist in ten ways, each on objects of its own,
-- kilometres from the others:
--   gun         eight tanks, four a side, 600 m apart, open fire
--   air         two attack aircraft, each against four trucks
--   shootdown   three aircraft flown over air defence vehicles
--   static_weapon  an attack aircraft bombs four statics (a heavier weapon)
--   explosion   trigger.action.explosion on each of four vehicles, on three
--               statics (static_explosion) and on the vehicles of `wreck`
--   wreck       Unit.destroy on the three exploded vehicles, @WRECK@ s after
--               the explosion, while they still burn
--   unit        Unit.destroy on five of six vehicles of a group
--   group       Group.destroy on five groups of one vehicle
--   static_remove  StaticObject.destroy on three statics
--   control     a sixth vehicle, and the attack aircraft, left alone
-- The probe keeps every world event but shots and hits (those it counts),
-- with its time and the object ID and category of its initiator and target.
-- Every 0.05 s it reads whether each watched object exists, timing those
-- calls; once a second, whether coalition.getGroups still lists it. It goes
-- on until every object that ceased to exist has been watched for @WATCH@ s
-- more, and at least @GIVE_UP@ s. Then it reads, on each ID that ceased,
-- what `Unit.isDead` or `StaticObject.getLife` answers (a removed object
-- raises), and destroys what it added.
-- states: server
-- timeout: 1200
local E = ...
local S = E.lib('scenario')
local WATCH, GIVE_UP, WRECK = @WATCH@, @GIVE_UP@, @WRECK@
local clock = E.clock

local NAMES, IDS = {}, {}
pcall(function()
  for name, id in pairs(world.event) do
    if type(id) == 'number' then
      NAMES[id], IDS[name] = name, id
    end
  end
end)
-- Counted for each object, not kept one by one: a fight sends hundreds.
local NOISY = { S_EVENT_SHOT = true, S_EVENT_HIT = true, S_EVENT_SHOOTING_START = true, S_EVENT_SHOOTING_END = true }

local origin = S.origin()
local RUSSIA = country.id.RUSSIA
local spawned, groups, statics = {}, {}, {}

-- Add a group through `fn`, and note whether DCS took it.
local function add(name, fn)
  local ok, group = pcall(fn)
  groups[#groups + 1] = name
  spawned[name] = ok and group ~= nil or tostring(group)
  return ok and group or nil
end

-- Add a static, the same way.
local STATIC_TYPE = 'Fuel tank'
local function add_static(name, north, east)
  local ok, static = pcall(S.spawn_static, name, north, east, STATIC_TYPE, 'Fortifications')
  statics[#statics + 1] = name
  spawned[name] = ok and static ~= nil or tostring(static)
end

local function row(prefix, n, x, z, step)
  local units = {}
  for i = 1, n do
    units[i] = { name = prefix .. i, x = x, z = z + (i - 1) * step, heading = 0 }
  end
  return units
end

-- gun: the fight of design-combat.lua, where it is known to end in kills,
-- with four tanks a side.
local fx, fz = origin.x + 1500, origin.z - 2500
local blue = add('np-deaths-blue', function()
  return S.ground('np-deaths-blue', 'M-1 Abrams', row('np-deaths-b', 4, fx, fz, 40))
end)
local red = add('np-deaths-red', function()
  local units = row('np-deaths-r', 4, fx + 600, fz, 40)
  for _, u in ipairs(units) do
    u.heading = math.pi
  end
  return S.ground('np-deaths-red', 'T-72B', units, nil, RUSSIA)
end)
-- explosion and the scripted removals, 10 km from the fight, each on a row
-- of its own.
local ex, ez = origin.x, origin.z + 8000
add('np-deaths-ex', function()
  return S.ground('np-deaths-ex', 'M-113', row('np-deaths-e', 4, ex, ez, 40))
end)
add('np-deaths-wr', function()
  return S.ground('np-deaths-wr', 'M-113', row('np-deaths-w', 3, ex + 300, ez, 40))
end)
for i = 1, 5 do
  add('np-deaths-gd' .. i, function()
    return S.ground('np-deaths-gd' .. i, 'M-113', { { name = 'np-deaths-gd' .. i .. '-1', x = ex + 600 + (i - 1) * 40, z = ez } })
  end)
end
add('np-deaths-ud', function()
  return S.ground('np-deaths-ud', 'M-113', row('np-deaths-u', 6, ex + 900, ez, 40))
end)
-- air: two groups of trucks 20 km east, out of the air defence's reach, each
-- with an attack aircraft 9 km south of it. The bomb's CLSID is UNVERIFIED;
-- with none loaded the aircraft has its gun.
local MK82 = '{BCE4E030-38E9-423E-98ED-24BE3DA87C32}'
local tx, tz = origin.x + 4000, origin.z + 20000
local function attacker(name, x, z)
  return add(name, function()
    return S.spawn_aircraft({
      name = name,
      kind = 'A-10A',
      task = 'CAS',
      x = x,
      z = z,
      alt = origin.y + 1500,
      speed = 150,
      heading = 0,
      gun = 100,
      pylons = { [3] = { CLSID = MK82 }, [4] = { CLSID = MK82 }, [8] = { CLSID = MK82 }, [9] = { CLSID = MK82 } },
    })
  end)
end
local trucks, trucks2
trucks = add('np-deaths-tgt', function()
  return S.ground('np-deaths-tgt', 'Ural-375', row('np-deaths-t', 4, tx, tz, 30), nil, RUSSIA)
end)
trucks2 = add('np-deaths-tgt2', function()
  return S.ground('np-deaths-tgt2', 'Ural-375', row('np-deaths-v', 4, tx + 3000, tz, 30), nil, RUSSIA)
end)
local cas = attacker('np-deaths-cas', tx - 9000, tz)
local cas2 = attacker('np-deaths-cas2', tx - 6000, tz)
-- static_weapon: four statics 20 km west, and an attack aircraft that bombs
-- each (a heavier weapon than a tank's gun, DECISIONS 2026-11-14 01:27 item 1).
local wx, wz = origin.x - 14000, origin.z + 20000
local bombed = {}
for i = 1, 4 do
  add_static('np-deaths-sw' .. i, wx - origin.x, wz - origin.z + (i - 1) * 60)
  bombed[i] = { x = wx, y = wz + (i - 1) * 60 }
end
local cas3 = attacker('np-deaths-cas3', wx - 9000, wz)
-- static_explosion and static_remove.
for i = 1, 3 do
  add_static('np-deaths-se' .. i, ex - origin.x + 1300, ez - origin.z + (i - 1) * 60)
  add_static('np-deaths-sr' .. i, ex - origin.x + 1600, ez - origin.z + (i - 1) * 60)
end
-- shootdown: the air defence scenario of the event catalogue
-- (lua/catalogue/30-air-defense.lua), where it is known to end in a kill,
-- with three aircraft one behind the other.
local dx, dz = origin.x - 6000, origin.z + 6000
local defence = {
  add('np-deaths-sam', function()
    return S.ground('np-deaths-sam', 'Osa 9A33 ln', { { name = 'np-deaths-sam-1', x = dx, z = dz } }, nil, RUSSIA)
  end),
  add('np-deaths-aaa', function()
    return S.ground('np-deaths-aaa', 'ZSU-23-4 Shilka', { { name = 'np-deaths-aaa-1', x = dx + 100, z = dz + 100 } },
      nil, RUSSIA)
  end),
}
for i = 1, 3 do
  add('np-deaths-victim' .. i, function()
    local z = dz + (i - 1) * 40
    return S.spawn_aircraft({
      name = 'np-deaths-victim' .. i,
      x = dx + 6000 + (i - 1) * 3000,
      z = z,
      alt = 1500,
      speed = 150,
      heading = math.pi,
      task = 'Nothing',
      points = { S.air_point(dx + 6000 + (i - 1) * 3000, z, 150, 1500), S.air_point(dx - 20000, z, 150, 1500) },
    })
  end)
end

-- Open fire (option 0, value 2), each under pcall.
local orders = {}
local function order(name, group, option, value)
  orders[name .. ':' .. option] = group ~= nil and (pcall(function()
    group:getController():setOption(option, value)
  end))
end
order('blue', blue, 0, 2)
order('red', red, 0, 2)
order('sam', defence[1], 0, 2)
order('aaa', defence[2], 0, 2)

-- The watched objects, by object ID and by name: name, stage, whether a
-- static.
local WATCHED = {}
local function watch(prefix, n, stage, static)
  for i = 1, n do
    WATCHED[#WATCHED + 1] = { prefix .. i, stage, static }
  end
end
watch('np-deaths-b', 4, 'gun')
watch('np-deaths-r', 4, 'gun')
watch('np-deaths-t', 4, 'air')
watch('np-deaths-v', 4, 'air')
for i = 1, 3 do
  WATCHED[#WATCHED + 1] = { 'np-deaths-victim' .. i .. '-1', 'shootdown' }
end
watch('np-deaths-sw', 4, 'static_weapon', true)
watch('np-deaths-e', 4, 'explosion')
watch('np-deaths-se', 3, 'static_explosion', true)
watch('np-deaths-w', 3, 'wreck')
watch('np-deaths-u', 5, 'unit')
for i = 1, 5 do
  WATCHED[#WATCHED + 1] = { 'np-deaths-gd' .. i .. '-1', 'group' }
end
watch('np-deaths-sr', 3, 'static_remove', true)
WATCHED[#WATCHED + 1] = { 'np-deaths-u6', 'control' }
for _, name in ipairs({ 'np-deaths-cas-1', 'np-deaths-cas2-1', 'np-deaths-cas3-1' }) do
  WATCHED[#WATCHED + 1] = { name, 'bystander' }
end
local by_id, by_name, order_of = {}, {}, {}
for _, entry in ipairs(WATCHED) do
  local name, stage, static = entry[1], entry[2], entry[3]
  local u = static and StaticObject.getByName(name) or Unit.getByName(name)
  local w = { name = name, stage = stage, static = static == true, unit = u, lives = {}, events = {}, noisy = {},
    alive = u ~= nil }
  if u then
    w.id = rawget(u, 'id_')
    w.type = u:getTypeName()
    by_id[w.id] = w
  end
  by_name[name] = w
  order_of[#order_of + 1] = w
end

-- One side of an event: the object's ID, and the category and unit name of
-- the plain table DCS adds beside it.
local function side(event, key)
  local o = rawget(event, key)
  local id = type(o) == 'table' and rawget(o, 'id_') or nil
  local d = rawget(event, key .. '_data')
  if type(d) == 'table' then
    return id, rawget(d, 'object_category'), rawget(d, 'unit_name')
  end
  return id
end

local log, counts, hard_end = {}, {}, nil
local handler = {}
function handler:onEvent(event)
  pcall(function()
    local t = timer.getTime()
    if hard_end and t > hard_end then
      return
    end
    local name = NAMES[rawget(event, 'id')] or tostring(rawget(event, 'id'))
    counts[name] = (counts[name] or 0) + 1
    local ii, ic, iname = side(event, 'initiator')
    local gi, gc, gname = side(event, 'target')
    local weapon = rawget(event, 'weapon_name')
    if not NOISY[name] and #log < 2500 then
      log[#log + 1] = { t = t, e = name, i = ii, ic = ic, g = gi, gc = gc, w = type(weapon) == 'string' and weapon or nil }
    end
    for _, as in ipairs({ 'initiator', 'target' }) do
      local id, uname = ii, iname
      if as == 'target' then
        id, uname = gi, gname
      end
      local w = id and by_id[id]
      local by = 'id'
      if not w and type(uname) == 'string' and by_name[uname] then
        w, by = by_name[uname], 'name'
      end
      if w then
        if NOISY[name] then
          local n = w.noisy[name .. ':' .. as] or { n = 0, first = t }
          n.n, n.last = n.n + 1, t
          n.weapon = type(weapon) == 'string' and weapon or n.weapon
          w.noisy[name .. ':' .. as] = n
        elseif #w.events < 60 then
          w.events[#w.events + 1] = { t = t, e = name, as = as, by = by, id = id }
        end
      end
    end
  end)
end
world.addEventHandler(handler)

-- The cost of `isExist` on one row, timed over the sampling's own calls:
-- these are the existence checks the agent makes (spec/records.md, "The
-- object table"). The clock's own resolution limits one call; the total
-- and the count give the mean.
local cost = { calls = 0, seconds = 0 }
local function sample(t)
  for _, w in ipairs(order_of) do
    if w.alive then
      local t0 = clock and clock()
      local ok, exists = pcall(w.unit.isExist, w.unit)
      if t0 then
        cost.calls, cost.seconds = cost.calls + 1, cost.seconds + (clock() - t0)
      end
      if not ok or not exists then
        w.alive, w.gone_at = false, t
      elseif not w.static then
        local got, life = pcall(w.unit.getLife, w.unit)
        if got and life ~= w.life_last and #w.lives < 20 then
          w.lives[#w.lives + 1] = { t = t, life = life }
        end
        w.life_last = got and life or w.life_last
      end
    end
  end
end

-- Which watched objects coalition.getGroups lists now. Statics are not in
-- a group.
local function listing(t)
  local listed = {}
  for s = 0, 2 do
    for _, g in ipairs(coalition.getGroups(s) or {}) do
      local ok, units = pcall(g.getUnits, g)
      for _, u in ipairs(ok and units or {}) do
        listed[rawget(u, 'id_')] = true
      end
    end
  end
  for _, w in ipairs(order_of) do
    if w.id and not w.static then
      if listed[w.id] then
        w.listed_last, w.unlisted_at = t, nil
      elseif not w.unlisted_at then
        w.unlisted_at = t
      end
    end
  end
end

local started = timer.getTime()
hard_end = started + GIVE_UP + WATCH + 120
sample(started)
listing(started)

local done = {}
local function once(key, after, t, fn)
  if not done[key] and t - started >= after then
    done[key] = (pcall(fn))
  end
end

local function explode(name)
  trigger.action.explosion(by_name[name].unit:getPoint(), 300)
end

-- Bombing tasks, one for each static the third aircraft is to bomb
-- (UNVERIFIED: the task's fields are the mission editor's).
local function bombing()
  local tasks = {}
  for i, point in ipairs(bombed) do
    tasks[i] = {
      number = i,
      id = 'Bombing',
      params = { point = point, weaponType = 2032, expend = 'One', attackQty = 1, groupAttack = false },
    }
  end
  cas3:getController():setTask({ id = 'ComboTask', params = { tasks = tasks } })
end

local explosion_at = math.min(20, GIVE_UP / 3)
local next_sample, next_listing = 0, 0
return E.wait(function()
  local t = timer.getTime()
  if t >= next_sample then
    sample(t)
    next_sample = t + 0.05
  end
  if t >= next_listing then
    listing(t)
    next_listing = t + 1
  end
  once('attack', math.min(2, GIVE_UP / 4), t, function()
    cas:getController():setTask({ id = 'AttackGroup', params = { groupId = trucks:getID() } })
    cas2:getController():setTask({ id = 'AttackGroup', params = { groupId = trucks2:getID() } })
  end)
  once('bombing', math.min(2, GIVE_UP / 4), t, bombing)
  once('explosion', explosion_at, t, function()
    for i = 1, 4 do
      explode('np-deaths-e' .. i)
    end
    for i = 1, 3 do
      explode('np-deaths-se' .. i)
      explode('np-deaths-w' .. i)
    end
  end)
  once('destroys', math.min(30, GIVE_UP / 2), t, function()
    for i = 1, 5 do
      Group.getByName('np-deaths-gd' .. i):destroy()
      by_name['np-deaths-u' .. i].unit:destroy()
    end
    for i = 1, 3 do
      by_name['np-deaths-sr' .. i].unit:destroy()
    end
  end)
  -- Unit.destroy on a vehicle that is still burning: it exploded @WRECK@ s ago.
  once('wrecks', explosion_at + WRECK, t, function()
    for i = 1, 3 do
      by_name['np-deaths-w' .. i].unit:destroy()
    end
  end)
  local finished = t - started >= GIVE_UP
  for _, w in ipairs(order_of) do
    if w.gone_at and t - w.gone_at < WATCH then
      finished = false
    end
  end
  if not finished and t < hard_end - 90 then
    return nil
  end
  sample(t)
  listing(t)
  hard_end = t
  world.removeEventHandler(handler)
  -- What each ID that ceased answers now: Unit.isDead or
  -- StaticObject.getLife on a plain table holding the ID. A killed unit
  -- answers; a removed one raises (LIVE, E38).
  local units = {}
  for _, w in ipairs(order_of) do
    for _, e in ipairs(w.events) do
      e.after_gone_s = w.gone_at and e.t - w.gone_at or nil
    end
    local ok, named = pcall(w.static and StaticObject.getByName or Unit.getByName, w.name)
    local read_end
    if w.gone_at and w.id then
      local bare = { id_ = w.id }
      local answered, value
      if w.static then
        answered, value = pcall(StaticObject.getLife, bare)
        read_end = answered and 'dead' or 'removed'
      else
        answered, value = pcall(Unit.isDead, bare)
        read_end = answered and (value and 'dead' or 'alive') or 'removed'
      end
    end
    units[w.name] = {
      id = w.id,
      stage = w.stage,
      static = w.static,
      type = w.type,
      found = w.unit ~= nil,
      gone_at = w.gone_at,
      gone_after_s = w.gone_at and w.gone_at - started or nil,
      watched_s = w.gone_at and t - w.gone_at or nil,
      lives = w.lives,
      events = w.events,
      noisy = w.noisy,
      listed_last = w.listed_last,
      unlisted_at = w.unlisted_at,
      unlisted_after_gone_s = w.unlisted_at and w.gone_at and w.unlisted_at - w.gone_at or nil,
      listed_at_end = w.id ~= nil and not w.static and w.unlisted_at == nil,
      by_name_at_end = ok and named ~= nil,
      read_at_end = read_end,
    }
  end
  local ammo = {}
  pcall(function()
    for _, entry in ipairs(by_name['np-deaths-cas-1'].unit:getAmmo() or {}) do
      ammo[#ammo + 1] = { type = entry.desc and entry.desc.typeName, count = entry.count }
    end
  end)
  for _, name in ipairs(groups) do
    S.destroy(name)
  end
  for _, name in ipairs(statics) do
    pcall(function()
      StaticObject.getByName(name):destroy()
    end)
  end
  return {
    started = started,
    ended = t,
    seconds = t - started,
    watch_s = WATCH,
    give_up_s = GIVE_UP,
    wreck_s = WRECK,
    spawned = spawned,
    orders = orders,
    actions = done,
    attacker_ammo_at_end = ammo,
    isexist_calls = cost.calls,
    isexist_us = cost.calls > 0 and cost.seconds / cost.calls * 1e6 or nil,
    units = units,
    counts = counts,
    event_ids = IDS,
    events = log,
  }
end, GIVE_UP + WATCH + 180, 'running')
