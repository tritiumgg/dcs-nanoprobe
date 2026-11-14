-- E38: does a read tell a death from a removal, for every kind of object?
--
-- For a ground unit, two reads on `{ id_ = <object ID> }` at the moment it
-- ceases to exist separate a kill (Unit.isDead true, Unit.getLife a number)
-- from a scripted removal (both raise). This snippet stages the same question
-- for the other kinds, several of each:
--   ground    Unit.destroy, Group.destroy, trigger.action.deactivateGroup,
--             an explosion beside the unit (it burns), and Unit.destroy on
--             the burning wreck;
--   statics   an explosion, tank fire at the point they stand on,
--             StaticObject.destroy;
--   cargo     the same three;
--   aircraft  an explosion in flight, air defense, no fuel, a landing,
--             Unit.destroy;
--   ships     an explosion, Unit.destroy.
-- Once a second of model time it reads every object and logs each change of
-- what the reads return, and it logs every world event that names one. The
-- report gives, for each case, how the reads classified the object when it
-- ceased to exist, whether it was ever seen to exist again, what the reads
-- returned afterwards, and how long the first death event took.
-- states: server
-- timeout: 460

local E = ...
local S = E.lib('scenario')
local RUN_S = 330

local function try(fn, ...)
  local ok, value = pcall(fn, ...)
  if ok then
    return value
  end
  return nil
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
    if #events < 6000 then
      events[#events + 1] = {
        t = timer.getTime() - started,
        name = event_names[event.id] or tostring(event.id),
        initiator = type(event.initiator) == 'table' and rawget(event.initiator, 'id_') or nil,
        target = type(event.target) == 'table' and rawget(event.target, 'id_') or nil,
      }
    end
  end,
})

-- Land cells 600 m apart, east of the first airbase, and one point at sea.
local origin = S.origin()
local cells = {}
for row = 0, 9 do
  for column = 0, 7 do
    local x, z = origin.x + 3000 + column * 600, origin.z + 6000 + row * 600
    local kind = try(function()
      return land.getSurfaceType({ x = x, y = z })
    end)
    if kind == nil or kind == 1 then
      cells[#cells + 1] = { x = x, z = z }
    end
  end
end
local next_cell = 0
local function cell()
  next_cell = next_cell + 1
  return assert(cells[next_cell], 'not enough land for the probe')
end
local sea
for ring = 1, 25 do
  for step = 0, 15 do
    local angle = step * math.pi / 8
    local x, z = origin.x + math.cos(angle) * ring * 3000, origin.z + math.sin(angle) * ring * 3000
    local water = true
    for i = 0, 9 do
      water = water and try(function()
        return land.getSurfaceType({ x = x, y = z + i * 200 })
      end) == 3
    end
    if water and not sea then
      sea = { x = x, z = z }
    end
  end
end

-- name -> the case it belongs to, what the case expects, and how to find it.
local subjects, order = {}, {}
local function subject(name, case, expect, static)
  subjects[name] = { case = case, expect = expect, static = static, steps = {} }
  order[#order + 1] = name
end

local function carrier(name, case, expect)
  local at = cell()
  S.ground(name, 'M-113', { { name = name .. '-1', x = at.x, z = at.z, heading = 0 } })
  subject(name .. '-1', case, expect)
  return at
end

local function static(name, case, expect, kind, category, at)
  local point = at or cell()
  S.spawn_static(name, point.x - origin.x, point.z - origin.z, kind, category)
  subject(name, case, expect, true)
  return point
end

-- Ground.
for i = 1, 5 do
  carrier('np-k-ud' .. i, 'ground / Unit.destroy', 'removed')
  carrier('np-k-gd' .. i, 'ground / Group.destroy', 'removed')
  carrier('np-k-x' .. i, 'ground / explosion beside it', 'dead')
end
for i = 1, 3 do
  carrier('np-k-da' .. i, 'ground / deactivateGroup', 'removed')
  carrier('np-k-xd' .. i, 'ground / explosion, then Unit.destroy on the wreck', 'dead')
end

-- Statics and cargo. One cell holds five depots for the tanks to fire at,
-- another three crates.
for i = 1, 5 do
  static('np-k-sx' .. i, 'static / explosion', 'dead', 'FARP Fuel Depot')
  static('np-k-cx' .. i, 'cargo / explosion', 'dead', 'ammo_cargo', 'Cargos')
end
for i = 1, 3 do
  static('np-k-sd' .. i, 'static / StaticObject.destroy', 'removed', 'FARP Fuel Depot')
  static('np-k-cd' .. i, 'cargo / StaticObject.destroy', 'removed', 'ammo_cargo', 'Cargos')
end
local depots, crates = cell(), cell()
for i = 1, 5 do
  static('np-k-sw' .. i, 'static / tank fire', 'dead', 'FARP Fuel Depot', nil, { x = depots.x + (i - 1) * 9, z = depots.z })
end
for i = 1, 3 do
  static('np-k-cw' .. i, 'cargo / tank fire', 'dead', 'ammo_cargo', 'Cargos', { x = crates.x + (i - 1) * 6, z = crates.z })
end
local gunners = {
  { S.ground('np-k-gun1', 'M-1 Abrams', {
    { name = 'np-k-gun1-1', x = depots.x + 18, z = depots.z - 500, heading = 0 },
    { name = 'np-k-gun1-2', x = depots.x + 40, z = depots.z - 500, heading = 0 },
  }), depots.x + 18, depots.z },
  { S.ground('np-k-gun2', 'M-1 Abrams', {
    { name = 'np-k-gun2-1', x = crates.x + 6, z = crates.z + 500, heading = 0 },
  }), crates.x + 6, crates.z },
}

-- Aircraft, 40 km and more from the air defense.
local function aircraft(name, case, expect, options)
  options.name = name
  try(S.spawn_aircraft, options)
  subject(name .. '-1', case, expect)
end
local south = { x = origin.x - 40000, z = origin.z + 20000 }
for i = 1, 3 do
  aircraft('np-k-ax' .. i, 'aircraft / explosion in flight', 'dead', { x = south.x, z = south.z + i * 2000, alt = 3000, heading = math.pi })
  aircraft('np-k-ad' .. i, 'aircraft / Unit.destroy', 'removed', { x = south.x - 5000, z = south.z + i * 2000, alt = 3000, heading = math.pi })
end
for i = 1, 2 do
  aircraft('np-k-af' .. i, 'aircraft / no fuel', 'dead', { x = south.x - 10000, z = south.z + i * 2000, alt = 600, heading = math.pi, fuel = 1 })
end
local defense = { x = origin.x + 40000, z = origin.z + 20000 }
local battery = {
  S.ground('np-k-sam', 'Osa 9A33 ln', {
    { name = 'np-k-sam-1', x = defense.x, z = defense.z },
    { name = 'np-k-sam-2', x = defense.x, z = defense.z + 300 },
  }, nil, country.id.RUSSIA),
  S.ground('np-k-aaa', 'ZSU-23-4 Shilka', {
    { name = 'np-k-aaa-1', x = defense.x + 150, z = defense.z + 150 },
    { name = 'np-k-aaa-2', x = defense.x - 150, z = defense.z + 150 },
  }, nil, country.id.RUSSIA),
}
for i = 1, 3 do
  local from = defense.x + 5000 + i * 3000
  aircraft('np-k-as' .. i, 'aircraft / air defense', 'dead', {
    x = from, z = defense.z, alt = 1500, speed = 150, heading = math.pi, task = 'Nothing',
    points = { S.air_point(from, defense.z, 150, 1500), S.air_point(defense.x - 30000, defense.z, 150, 1500) },
  })
end
local field = S.airdrome()
if field then
  aircraft('np-k-al1', 'aircraft / lands', 'unknown', {
    kind = 'UH-1H', category = Group.Category.HELICOPTER, x = field.x + 2500, z = field.z + 2500, alt = 300, speed = 50,
    fuel = 400, task = 'Nothing',
    points = { S.air_point(field.x + 2500, field.z + 2500, 50, 300), S.airdrome_point('land', field) },
  })
end

-- Ships, when the map has open water within 75 km.
if sea then
  for i = 1, 8 do
    local name = 'np-k-h' .. i
    local x, z = sea.x, sea.z + (i - 1) * 200
    try(coalition.addGroup, country.id.USA, Group.Category.SHIP, {
      name = name, task = 'Nothing', visible = false, start_time = 0, x = x, y = z,
      units = { { name = name .. '-1', type = 'speedboat', x = x, y = z, heading = 0, skill = 'Average' } },
      route = { points = { { x = x, y = z, type = 'Turning Point', action = 'Turning Point', speed = 0, alt = 0, ETA = 0 } } },
    })
    S.groups[name] = true
    subject(name .. '-1', i <= 5 and 'ship / explosion' or 'ship / Unit.destroy', i <= 5 and 'dead' or 'removed')
  end
end

-- What the reads return for one object, as short text. A living object's
-- life is written `n`, so damage alone is not a change.
local function mark(fn, ...)
  local ok, value = pcall(fn, ...)
  if not ok then
    return 'x', nil
  end
  if type(value) == 'boolean' then
    return value and '1' or '0', value
  end
  if type(value) == 'number' then
    return string.format('%.3g', value), value
  end
  return value == nil and 'nil' or 't', value
end

local function reads(object, name)
  local handle = object.handle
  local exists = mark(Object.isExist, handle)
  local dead = mark(Unit.isDead, handle)
  local life, life_value = mark(Unit.getLife, handle)
  local parts = { 'E' .. exists, 'D' .. dead, 'L' .. ((exists == '1' and life_value) and 'n' or life) }
  local own_life
  if object.static then
    local text, value = mark(StaticObject.getLife, handle)
    own_life = text
    parts[#parts + 1] = 'S' .. ((exists == '1' and value) and 'n' or text)
  end
  parts[#parts + 1] = 'P' .. mark(Object.getPoint, handle)
  parts[#parts + 1] = 'C' .. mark(Object.getCategory, handle)
  local _, x = mark(DCS.getUnitPositionByObjectId, object.id)
  parts[#parts + 1] = 'G' .. ((x and x ~= 0) and '1' or '0')
  parts[#parts + 1] = 'N' .. mark(function()
    return (object.static and StaticObject.getByName or Unit.getByName)(name) ~= nil
  end)
  -- The two-read rule, and for a static the same rule on its own getLife.
  local class
  if object.static then
    class = (own_life == 'x' and 'REMOVED') or 'DEAD'
  elseif dead == '1' and life ~= 'x' then
    class = 'DEAD'
  elseif dead == 'x' and life == 'x' then
    class = 'REMOVED'
  else
    class = 'OTHER'
  end
  return exists, table.concat(parts, ' '), class
end

local function sample(now)
  for _, name in ipairs(order) do
    local object = subjects[name]
    if not object.id then
      local found = try(object.static and StaticObject.getByName or Unit.getByName, name)
      if found then
        object.id = rawget(found, 'id_')
        object.handle = { id_ = object.id }
      end
    end
    if object.id then
      local exists, text, class = reads(object, name)
      if text ~= object.last then
        object.last = text
        if #object.steps < 12 then
          object.steps[#object.steps + 1] = { t = now, text = text }
        end
      end
      if exists ~= '1' and not object.gone_at then
        object.gone_at, object.class = now, class
      elseif exists == '1' and object.gone_at then
        object.revived = true
      end
    end
  end
end

local DEATH = { S_EVENT_DEAD = true, S_EVENT_UNIT_LOST = true, S_EVENT_CRASH = true }

local function report()
  local cases, case_order = {}, {}
  for _, name in ipairs(order) do
    local object = subjects[name]
    local case = cases[object.case]
    if not case then
      case = {
        expect = object.expect, objects = 0, never_found = 0, ceased = 0, classes = {}, revived = 0,
        death_event = 0, death_event_s = {}, after = {}, events = {}, still_there = {},
      }
      cases[object.case] = case
      case_order[#case_order + 1] = object.case
    end
    case.objects = case.objects + 1
    if not object.id then
      case.never_found = case.never_found + 1
    elseif not object.gone_at then
      case.still_there[object.last] = (case.still_there[object.last] or 0) + 1
    else
      case.ceased = case.ceased + 1
      case.classes[object.class] = (case.classes[object.class] or 0) + 1
      case.revived = case.revived + (object.revived and 1 or 0)
      local first
      for _, event in ipairs(events) do
        if (event.initiator == object.id or event.target == object.id) and event.t >= object.gone_at - 2 then
          case.events[event.name] = (case.events[event.name] or 0) + 1
          if not first and DEATH[event.name] and event.initiator == object.id then
            first = event.t - object.gone_at
          end
        end
      end
      if first then
        case.death_event = case.death_event + 1
        case.death_event_s[#case.death_event_s + 1] = math.floor(first + 0.5)
      end
      -- What the reads returned from then on, each change with its second.
      local trail = {}
      for _, step in ipairs(object.steps) do
        if step.t >= object.gone_at then
          trail[#trail + 1] = string.format('+%d %s', math.floor(step.t - object.gone_at + 0.5), step.text)
        end
      end
      local text = table.concat(trail, ' > ')
      case.after[text] = (case.after[text] or 0) + 1
    end
  end
  for _, name in ipairs(case_order) do
    E.observe(name, cases[name], { info = true })
  end
  E.observe('cases', case_order, { info = true })
  E.observe('sea_found', sea ~= nil, { info = true })
end

local done = {}
local function once(name, at, fn)
  if not done[name] and timer.getTime() - started >= at then
    done[name] = true
    local failed = {}
    fn(function(label, action, ...)
      if not pcall(action, ...) then
        failed[#failed + 1] = label
      end
    end)
    E.observe(name, #failed == 0 and 'done' or failed, { info = true })
  end
end

local function explode(name, is_static, power, offset)
  local object = (is_static and StaticObject.getByName or Unit.getByName)(name)
  local p = object:getPoint()
  trigger.action.explosion({ x = p.x, y = p.y, z = p.z - (offset or 0) }, power)
end

local next_sample = started
return E.wait(function()
  local now = timer.getTime()
  once('orders', 1, function(act)
    for _, group in ipairs(battery) do
      act('open fire', S.open_fire, group)
    end
    for _, gunner in ipairs(gunners) do
      act('fire at point', function()
        gunner[1]:getController():setTask({
          id = 'FireAtPoint',
          params = { x = gunner[2], y = gunner[3], radius = 20, expendQty = 30, expendQtyEnabled = true },
        })
      end)
    end
  end)
  once('explosions', 5, function(act)
    for i = 1, 5 do
      act('x' .. i, explode, 'np-k-x' .. i .. '-1', false, 30, 6)
      act('sx' .. i, explode, 'np-k-sx' .. i, true, 1000)
      act('cx' .. i, explode, 'np-k-cx' .. i, true, 3000)
      if sea then
        act('h' .. i, explode, 'np-k-h' .. i .. '-1', false, 1000)
      end
    end
    for i = 1, 3 do
      act('xd' .. i, explode, 'np-k-xd' .. i .. '-1', false, 30, 6)
    end
  end)
  once('removals', 10, function(act)
    for i = 1, 5 do
      act('ud' .. i, function() Unit.getByName('np-k-ud' .. i .. '-1'):destroy() end)
      act('gd' .. i, function() Group.getByName('np-k-gd' .. i):destroy() end)
    end
    for i = 1, 3 do
      act('da' .. i, function() trigger.action.deactivateGroup(Group.getByName('np-k-da' .. i)) end)
      act('sd' .. i, function() StaticObject.getByName('np-k-sd' .. i):destroy() end)
      act('cd' .. i, function() StaticObject.getByName('np-k-cd' .. i):destroy() end)
      act('ad' .. i, function() Unit.getByName('np-k-ad' .. i .. '-1'):destroy() end)
      if sea then
        act('hd' .. i, function() Unit.getByName('np-k-h' .. (5 + i) .. '-1'):destroy() end)
      end
    end
  end)
  once('explosions_in_flight', 15, function(act)
    for i = 1, 3 do
      act('ax' .. i, explode, 'np-k-ax' .. i .. '-1', false, 50)
    end
  end)
  once('destroy_the_wrecks', 20, function(act)
    for i = 1, 3 do
      act('xd' .. i, function() Unit.destroy(subjects['np-k-xd' .. i .. '-1'].handle) end)
    end
  end)
  if now >= next_sample then
    next_sample = now + 1
    sample(now - started)
  end
  if now - started >= RUN_S then
    report()
    S.clear()
    return 'done'
  end
  return nil
end, 440, 'running')
