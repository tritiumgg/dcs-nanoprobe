-- E38: statics and an aircraft, as coverage.lua does for ground units.
--
--   - four statics: two fuel depots, one killed by an explosion and one
--     removed by StaticObject.destroy; a cargo crate killed by an explosion;
--     a windsock left alone;
--   - one fighter with flares, chaff and gun rounds: what getAmmo lists for
--     it, then a weak explosion at its position (damage) and a strong one
--     (a kill in flight).
-- Ten times a second of model time the snippet reads each object's life and
-- whether it exists, by a plain table `{ id_ = <object ID> }`, and logs each
-- change with the events that named the object in the second before. For an
-- object that ceases to exist it records what each read returns at once and
-- five seconds later, and every later event that names it with its delay.
-- states: server
-- timeout: 200

local E = ...
local S = E.lib('scenario')
local RUN_S, STEP_S, WINDOW_S = 120, 0.1, 1.0

local function try(fn, ...)
  local ok, value = pcall(fn, ...)
  if ok then
    return value
  end
  return nil
end

local function text(fn, ...)
  local ok, value = pcall(fn, ...)
  if not ok then
    return 'raises'
  end
  return type(value) == 'table' and 'table' or tostring(value)
end

local event_names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then
    event_names[id] = name
  end
end

local started = timer.getTime()
local events = {}
E.track.handler({
  onEvent = function(_, event)
    if #events < 2000 then
      events[#events + 1] = {
        t = timer.getTime() - started,
        name = event_names[event.id] or tostring(event.id),
        initiator = type(event.initiator) == 'table' and rawget(event.initiator, 'id_') or nil,
        target = type(event.target) == 'table' and rawget(event.target, 'id_') or nil,
      }
    end
  end,
})

local origin = S.origin()
local east = 6000
for _, candidate in ipairs({ 6000, 10000, 15000, 20000, 30000 }) do
  local kind = try(function()
    return land.getSurfaceType({ x = origin.x + 6000, y = origin.z + candidate })
  end)
  if kind == nil or kind == 1 then
    east = candidate
    break
  end
end

-- name -> { static, find, life }: how to find the object and read its life.
local tracked, order = {}, {}
local function track(name, static)
  tracked[name] = {
    static = static,
    find = static and StaticObject.getByName or Unit.getByName,
    life_of = static and StaticObject.getLife or Unit.getLife,
  }
  order[#order + 1] = name
end

S.spawn_static('np-e38-s1', 6000, east, 'FARP Fuel Depot')
S.spawn_static('np-e38-s2', 6000, east + 200, 'FARP Fuel Depot')
S.spawn_static('np-e38-s3', 6000, east + 400, 'ammo_cargo', 'Cargos')
S.spawn_static('np-e38-s4', 6000, east + 600)
for i = 1, 4 do
  track('np-e38-s' .. i, true)
end

-- The fighter, with countermeasures, which the library's helper leaves out.
local fx, fz = origin.x + 6000, origin.z + east + 5000
local function point(i)
  return {
    x = fx + i * 40000, y = fz, alt = 3000, alt_type = 'BARO', type = 'Turning Point', action = 'Turning Point',
    speed = 220, speed_locked = true, ETA = 0, ETA_locked = false, formation_template = '',
    task = { id = 'ComboTask', params = { tasks = {} } },
  }
end
try(coalition.addGroup, country.id.USA, Group.Category.AIRPLANE, {
  name = 'np-e38-j', task = 'CAP', start_time = 0, x = fx, y = fz,
  units = { {
    name = 'np-e38-j1', type = 'F-15C', x = fx, y = fz, alt = 3000, alt_type = 'BARO', speed = 220, heading = 0,
    skill = 'High', payload = { fuel = 3000, gun = 100, flare = 60, chaff = 60, pylons = {} },
    callsign = { 1, 1, 1, name = 'Enfield11' },
  } },
  route = { points = { point(0), point(1), point(2) } },
})
S.groups['np-e38-j'] = true
track('np-e38-j1', false)

local changes = {}
local function post_mortem(object, name)
  local handle, out = object.handle, {}
  out.getLife = text(object.life_of, handle)
  out.isExist = text(Object.isExist, handle)
  out.getPoint = text(Object.getPoint, handle)
  out.getCategory = text(Object.getCategory, handle)
  out.getDesc = text(object.static and StaticObject.getDesc or Unit.getDesc, handle)
  out.getByName = text(function()
    return object.find(name) ~= nil
  end)
  out.unit_isDead = text(function()
    return Unit.isDead(handle)
  end)
  out.position_getter = text(function()
    return DCS.getUnitPositionByObjectId(object.id)
  end)
  return out
end

local function sample(now)
  for _, name in ipairs(order) do
    local object = tracked[name]
    if not object.id then
      local found = try(object.find, name)
      if found then
        object.id = rawget(found, 'id_')
        object.handle = { id_ = object.id }
        object.life0 = try(object.life_of, object.handle)
        object.life = object.life0
        object.category = text(Object.getCategory, object.handle)
      end
    elseif object.gone then
      if not object.later and now >= object.gone_at + 5 then
        object.later = post_mortem(object, name)
      end
    else
      local exists = try(Object.isExist, object.handle)
      local life = exists and try(object.life_of, object.handle) or -1
      if life == nil then
        life = -1
      end
      if life ~= object.life then
        if #changes < 400 then
          changes[#changes + 1] = { t = now, name = name, from = object.life, to = life }
        end
        object.life = life
        if life == -1 then
          object.gone, object.gone_at = true, now
          object.at_once = post_mortem(object, name)
        end
      end
    end
  end
end

local function report()
  local lines, gone = {}, {}
  for _, c in ipairs(changes) do
    local id, seen = tracked[c.name].id, {}
    for _, event in ipairs(events) do
      if event.t >= c.t - WINDOW_S and event.t <= c.t + STEP_S then
        if event.initiator == id then
          seen[#seen + 1] = event.name .. '(i)'
        elseif event.target == id then
          seen[#seen + 1] = event.name .. '(t)'
        end
      end
    end
    lines[#lines + 1] = string.format('%.1f %s %s>%s [%s]', c.t, c.name, tostring(c.from), tostring(c.to), table.concat(seen, ','))
  end
  for _, name in ipairs(order) do
    local object = tracked[name]
    local after = {}
    if object.gone_at then
      for _, event in ipairs(events) do
        local role = (event.initiator == object.id and 'i') or (event.target == object.id and 't') or nil
        if role and event.t >= object.gone_at - WINDOW_S then
          after[#after + 1] = string.format('%s(%s)%+.1f', event.name, role, event.t - object.gone_at)
        end
      end
    end
    gone[name] = {
      life0 = object.life0, life = object.life, category = object.category, gone_at = object.gone_at,
      events = after, at_once = object.at_once, later = object.later,
    }
  end
  local counts = {}
  for _, event in ipairs(events) do
    counts[event.name] = (counts[event.name] or 0) + 1
  end
  E.observe('changes', lines, { info = true })
  E.observe('objects', gone, { info = true })
  E.observe('event_counts', counts, { info = true })
end

local done = {}
local function once(name, at, fn)
  if not done[name] and timer.getTime() - started >= at then
    done[name] = true
    E.observe(name, try(fn) or 'failed or nothing returned', { info = true })
  end
end

local function explode_at(name, static, power)
  local object = (static and StaticObject.getByName or Unit.getByName)(name)
  trigger.action.explosion(object:getPoint(), power)
  return 'done'
end

local next_sample = started
return E.wait(function()
  local now = timer.getTime()
  once('fighter_ammo', 3, function()
    local rows = {}
    for _, entry in ipairs(Unit.getAmmo(tracked['np-e38-j1'].handle) or {}) do
      rows[#rows + 1] = { name = entry.desc and entry.desc.typeName, category = entry.desc and entry.desc.category, count = entry.count }
    end
    return rows
  end)
  once('static_explosions', 5, function()
    explode_at('np-e38-s1', true, 1000)
    return explode_at('np-e38-s3', true, 1000)
  end)
  once('static_removal', 10, function()
    StaticObject.getByName('np-e38-s2'):destroy()
    return 'done'
  end)
  once('fighter_damage', 15, function()
    return explode_at('np-e38-j1', false, 5)
  end)
  once('fighter_kill', 30, function()
    return explode_at('np-e38-j1', false, 1000)
  end)
  if now >= next_sample then
    next_sample = now + STEP_S
    sample(now - started)
  end
  if now - started >= RUN_S then
    report()
    S.clear()
    return 'done'
  end
  return nil
end, 180, 'running')
