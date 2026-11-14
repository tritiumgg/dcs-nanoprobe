-- E38: which changes of a unit's life and ammunition does an event announce?
--
-- Four stages run side by side, far enough apart not to touch each other:
--   A  a scripted explosion beside a row of carriers (the control);
--   B  howitzers ordered to fire at a point among parked trucks: splash from
--      a real weapon;
--   C  a tank against three carriers, both ordered to open fire: direct
--      hits, what life does after a hit, and ammunition; a supply truck
--      stands by the tank for a rearm;
--   D  two carriers removed by script, one by Unit.destroy and one by
--      Group.destroy (the controls for a death); a third stays;
--   E  one fighter, to see what getAmmo lists for a gun.
-- Every world event is logged with its initiator's and target's object ID.
-- Ten times a second of model time the snippet reads each unit's life, its
-- state flags, and for the shooters the sum of getAmmo's counts, and logs
-- each change. At the end each change is matched with the events that
-- named the unit from one second before the sample that saw it to a tenth
-- of a second after. When an event names a target, the handler also reads
-- whether that target exists at that moment, which a sampler cannot see.
--
-- For a unit that ceases to exist the snippet also records what each read
-- returns for its object ID at once and five seconds later, and every later
-- event that names it with its delay, so a death can be told from a
-- scripted removal and the lateness of the death events is on record.
--
-- The `server` state also holds ED's debriefing event store
-- (DebriefingEventsData, ED-CODE: Scripts/UI/DebriefingEventsData.lua), which
-- DCS feeds through `addEvent`. The snippet wraps that function while it
-- runs, to log when each debriefing event arrives, and puts the original
-- back at the end.
-- states: server
-- timeout: 480

local E = ...
local S = E.lib('scenario')
local RUN_S = 360
local STEP_S = 0.1
local WINDOW_S = 1.0

local event_names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then
    event_names[id] = name
  end
end

local function try(fn, ...)
  local ok, value = pcall(fn, ...)
  if ok then
    return value
  end
  return nil
end

local started = timer.getTime()
local events, keysets, unnamed = {}, {}, {}
E.track.handler({
  onEvent = function(_, event)
    local id = event.id
    local name = event_names[id] or tostring(id)
    local initiator = type(event.initiator) == 'table' and rawget(event.initiator, 'id_') or nil
    local target = type(event.target) == 'table' and rawget(event.target, 'id_') or nil
    local keys = keysets[name] or {}
    keysets[name] = keys
    for key, value in pairs(event) do
      keys[tostring(key)] = type(value)
    end
    if not initiator and not target then
      unnamed[name] = (unnamed[name] or 0) + 1
    end
    if #events < 4000 then
      events[#events + 1] = {
        t = timer.getTime(),
        name = name,
        initiator = initiator,
        target = target,
        stamp = tonumber(event.time),
        -- Does the object the event is about exist as the event arrives?
        target_exists = target and try(Object.isExist, { id_ = target }),
        initiator_exists = initiator and try(Object.isExist, { id_ = initiator }),
      }
      -- `t` is model time; a change's time is relative to `started`.
    end
  end,
})

-- A base point where every site is on land.
local origin = S.origin()
local SITES = { { 0, 0 }, { -1500, -1500 }, { 4000, -2000 }, { -2000, -4000 }, { 0, 1500 }, { 700, 1500 }, { -3500, 1500 } }
local base
for _, east in ipairs({ 6000, 10000, 15000, 20000, 30000 }) do
  local all = true
  for _, site in ipairs(SITES) do
    local kind = try(function()
      return land.getSurfaceType({ x = origin.x + 3000 + site[1], y = origin.z + east + site[2] })
    end)
    all = all and (kind == nil or kind == 1)
  end
  if all then
    base = { x = origin.x + 3000, z = origin.z + east }
    break
  end
end
base = base or { x = origin.x + 3000, z = origin.z + 6000 }

local tracked, order = {}, {}
local function track(stage, name, ammo)
  tracked[name] = { stage = stage, ammo_watch = ammo }
  order[#order + 1] = name
end

local function row(prefix, n, x, z, dx, dz)
  local units = {}
  for i = 1, n do
    units[i] = { name = prefix .. i, x = x + (i - 1) * dx, z = z + (i - 1) * dz, heading = 0 }
  end
  return units
end

-- A: the control.
local ax, az = base.x - 1500, base.z - 1500
S.ground('np-e38-a', 'M-113', row('np-e38-a', 5, ax, az, 0, 8))
for i = 1, 5 do
  track('A explosion', 'np-e38-a' .. i)
end

-- B: trucks in a 3 by 3 grid, and the battery 6 km away.
local bx, bz = base.x + 4000, base.z - 2000
local trucks = {}
for i = 0, 8 do
  trucks[#trucks + 1] = { name = 'np-e38-b' .. (i + 1), x = bx + (i % 3) * 10, z = bz + math.floor(i / 3) * 10, heading = 0 }
  track('B artillery', 'np-e38-b' .. (i + 1))
end
S.ground('np-e38-b', 'Ural-375', trucks, nil, country.id.RUSSIA)
local battery = S.ground('np-e38-g', 'M-109', row('np-e38-g', 3, base.x - 2000, base.z - 4000, 0, 30))
for i = 1, 3 do
  track('B battery', 'np-e38-g' .. i, true)
end

-- C: direct fire, and a supply truck by the tank.
local cx, cz = base.x, base.z + 1500
local tank = S.ground('np-e38-t', 'M-1 Abrams', { { name = 'np-e38-t1', x = cx, z = cz, heading = 0 } })
S.ground('np-e38-s', 'M 818', { { name = 'np-e38-s1', x = cx - 25, z = cz, heading = 0 } })
local carriers = S.ground('np-e38-c', 'BTR-80', row('np-e38-c', 3, cx + 700, cz, 0, 25), nil, country.id.RUSSIA)
track('C tank', 'np-e38-t1', true)
track('C supply', 'np-e38-s1')
for i = 1, 3 do
  track('C carrier', 'np-e38-c' .. i, true)
end

-- D: the controls, removed by script.
local dx, dz = base.x - 3500, base.z + 1500
S.ground('np-e38-d', 'M-113', row('np-e38-d', 2, dx, dz, 0, 8))
S.ground('np-e38-e', 'M-113', { { name = 'np-e38-d3', x = dx, z = dz + 40, heading = 0 } })
for i = 1, 3 do
  track('D removed', 'np-e38-d' .. i)
end

-- E: a fighter far above it all.
try(S.spawn_aircraft, { name = 'np-e38-f', x = base.x, z = base.z + 20000, alt = 8000 })
track('E fighter', 'np-e38-f-1', true)

-- The debriefing store's arrivals. The `t` an entry arrives with is model
-- time (LIVE); the store keeps it as seconds since midnight.
local debrief, restore_debrief = {}, function() end
try(function()
  local store = DebriefingEventsData
  local original = store.addEvent
  store.addEvent = function(event, ...)
    pcall(function()
      if #debrief < 4000 and type(event) == 'table' then
        debrief[#debrief + 1] = {
          arrived = timer.getTime(),
          t = tonumber(event.t) or 0,
          name = tostring(event.type),
          initiator = event.initiator_object_id,
          target = event.target_object_id,
        }
      end
    end)
    return original(event, ...)
  end
  restore_debrief = function()
    store.addEvent = original
  end
  E.track.defer('e38 debriefing store', restore_debrief)
end)

local changes = {}
local function change(now, unit, kind, from, to)
  if #changes < 3000 then
    changes[#changes + 1] = { t = now, unit = unit, kind = kind, from = from, to = to }
  end
end

local FLAGS = { 'isBroken', 'isDead', 'isEffective', 'isAlive' }
local function flags(handle)
  local text = ''
  for _, name in ipairs(FLAGS) do
    local value = try(function()
      return Unit[name](handle)
    end)
    text = text .. (value == nil and '-' or (value and '1' or '0'))
  end
  return text
end

local function ammo_rows(handle)
  local rows, total = {}, 0
  local ammo = try(Unit.getAmmo, handle)
  if type(ammo) == 'table' then
    for _, entry in ipairs(ammo) do
      local count = tonumber(entry.count) or 0
      total = total + count
      rows[#rows + 1] = { name = type(entry.desc) == 'table' and entry.desc.typeName or '?', count = count }
    end
  end
  return total, rows
end

-- What each read returns for an object ID, as short text.
local function text(fn, ...)
  local ok, value = pcall(fn, ...)
  if not ok then
    return 'raises'
  end
  if type(value) == 'table' then
    return 'table'
  end
  return tostring(value)
end

local function post_mortem(unit, name)
  local handle, id = unit.handle, unit.id
  local out = {
    getLife = text(Unit.getLife, handle),
    isExist = text(Object.isExist, handle),
    getPoint = text(Object.getPoint, handle),
    getName = text(Unit.getName, handle),
    getCategory = text(Object.getCategory, handle),
    getDesc = text(Unit.getDesc, handle),
    getByName = text(function()
      return Unit.getByName(name) ~= nil
    end),
    position_getter = text(function()
      return DCS.getUnitPositionByObjectId(id)
    end),
    export_object = text(function()
      return Export.LoGetObjectById(id)
    end),
  }
  for _, flag in ipairs({ 'isBroken', 'isDead', 'isEffective', 'isAlive', 'isActive' }) do
    out[flag] = text(function()
      return Unit[flag](handle)
    end)
  end
  return out
end

local function sample(now)
  for _, name in ipairs(order) do
    local unit = tracked[name]
    if not unit.id then
      local object = try(Unit.getByName, name)
      if object then
        unit.id = rawget(object, 'id_')
        unit.handle = { id_ = unit.id }
        unit.life0 = try(Unit.getLife, unit.handle)
        unit.life = unit.life0
        unit.flags = flags(unit.handle)
        if unit.ammo_watch then
          unit.ammo, unit.ammo0_rows = ammo_rows(unit.handle)
          unit.ammo0 = unit.ammo
        end
      end
    elseif unit.gone then
      if not unit.later and now >= unit.gone_at + 5 then
        unit.later = post_mortem(unit, name)
      end
    else
      local life = try(Unit.getLife, unit.handle)
      if try(Object.isExist, unit.handle) == false or life == nil then
        life = -1
      end
      if life ~= unit.life then
        change(now, name, 'life', unit.life, life)
        unit.life = life
        unit.gone = life == -1
        if unit.gone then
          unit.gone_at = now
          unit.at_once = post_mortem(unit, name)
        end
      end
      if not unit.gone then
        local now_flags = flags(unit.handle)
        if now_flags ~= unit.flags then
          change(now, name, 'flags', unit.flags, now_flags)
          unit.flags = now_flags
        end
        if unit.ammo_watch then
          local total = ammo_rows(unit.handle)
          if total ~= unit.ammo then
            change(now, name, 'ammo', unit.ammo, total)
            unit.ammo = total
          end
        end
      end
    end
  end
end

-- The events that named `id` around the sample at `t`, as a sorted list of
-- names, each with the role it had, (i)nitiator or (t)arget, and how long
-- before the sample the nearest one came (negative: after it). A change of ammunition counts only
-- the events that named the unit as initiator.
local function named(id, t, as_target)
  local seen = {}
  for _, event in ipairs(events) do
    local at = event.t - started
    if at >= t - WINDOW_S and at <= t + STEP_S then
      local ms = math.floor((t - at) * 1000 + 0.5)
      local key = (event.initiator == id and event.name .. '(i)') or (as_target and event.target == id and event.name .. '(t)')
      -- Of several events of one kind, keep the one nearest the sample.
      if key and (not seen[key] or math.abs(ms) < math.abs(seen[key])) then
        seen[key] = ms
      end
    end
  end
  local list = {}
  for name, ms in pairs(seen) do
    list[#list + 1] = name .. '@' .. ms .. 'ms'
  end
  table.sort(list)
  return list
end

local function report()
  local by_stage, lines = {}, {}
  for _, c in ipairs(changes) do
    local unit = tracked[c.unit]
    local stage = by_stage[unit.stage] or {}
    by_stage[unit.stage] = stage
    local as_target = c.kind ~= 'ammo'
    local list = named(unit.id, c.t, as_target)
    local key = c.kind .. (#list > 0 and ' with event' or ' no event')
    stage[key] = (stage[key] or 0) + 1
    if c.kind == 'ammo' and c.to > c.from then
      stage['ammo rose'] = (stage['ammo rose'] or 0) + 1
    end
    if #lines < 160 then
      lines[#lines + 1] = string.format('%.1f %s %s %s>%s [%s]', c.t, c.unit, c.kind, tostring(c.from), tostring(c.to), table.concat(list, ','))
    end
  end
  local counts = {}
  for _, event in ipairs(events) do
    counts[event.name] = (counts[event.name] or 0) + 1
  end
  local ends = {}
  for _, name in ipairs(order) do
    local unit = tracked[name]
    ends[name] = { life0 = unit.life0, life = unit.life, flags = unit.flags, ammo0 = unit.ammo0, ammo = unit.ammo }
  end
  -- Every unit that ceased to exist: the events that named it from a second
  -- before, each with its delay in seconds, and the reads afterwards.
  local deaths = {}
  for _, name in ipairs(order) do
    local unit = tracked[name]
    if unit.gone_at then
      local after = {}
      for _, event in ipairs(events) do
        local role = (event.initiator == unit.id and 'i') or (event.target == unit.id and 't') or nil
        if role and event.t - started >= unit.gone_at - WINDOW_S then
          after[#after + 1] = string.format('%s(%s)%+.1f', event.name, role, event.t - started - unit.gone_at)
        end
      end
      local stored = {}
      for _, event in ipairs(debrief) do
        local role = (event.initiator == unit.id and 'i') or (event.target == unit.id and 't') or nil
        if role and event.arrived - started >= unit.gone_at - WINDOW_S then
          stored[#stored + 1] = string.format('%s(%s)%+.1f', event.name, role, event.arrived - started - unit.gone_at)
        end
      end
      deaths[name] = { stage = unit.stage, gone_at = unit.gone_at, events = after, debriefing = stored, at_once = unit.at_once, later = unit.later }
    end
  end
  -- How long after the time it carries each kind of event arrived, at most.
  local late = { world = {}, debriefing = {} }
  for _, event in ipairs(events) do
    if event.stamp then
      late.world[event.name] = math.max(late.world[event.name] or 0, event.t - event.stamp)
    end
  end
  local stored_counts = {}
  for _, event in ipairs(debrief) do
    late.debriefing[event.name] = math.max(late.debriefing[event.name] or 0, event.arrived - event.t)
    stored_counts[event.name] = (stored_counts[event.name] or 0) + 1
  end
  E.observe('arrived_after_its_own_time_s', late, { info = true })
  -- For each kind of event: how often its target, and its initiator, still
  -- existed when it arrived.
  local existed = {}
  for _, event in ipairs(events) do
    local row = existed[event.name] or { target_existed = 0, target_gone = 0, initiator_existed = 0, initiator_gone = 0 }
    existed[event.name] = row
    if event.target_exists ~= nil then
      local key = event.target_exists and 'target_existed' or 'target_gone'
      row[key] = row[key] + 1
    end
    if event.initiator_exists ~= nil then
      local key = event.initiator_exists and 'initiator_existed' or 'initiator_gone'
      row[key] = row[key] + 1
    end
  end
  E.observe('existed_when_the_event_came', existed, { info = true })
  E.observe('debriefing_counts', stored_counts, { info = true })
  E.observe('gone', deaths, { info = true })
  E.observe('changes_by_stage', by_stage, { info = true })
  E.observe('changes', lines, { info = true })
  E.observe('event_counts', counts, { info = true })
  E.observe('event_keys', keysets, { info = true })
  E.observe('events_naming_no_object', unnamed, { info = true })
  E.observe('units_at_end', ends, { info = true })
end

local next_sample = started
local done = {}
local function once(name, at, fn)
  if not done[name] and timer.getTime() - started >= at then
    done[name] = true
    E.observe(name, try(fn) == nil and 'failed or nothing returned' or 'done', { info = true })
  end
end

return E.wait(function()
  local now = timer.getTime()
  once('orders', 1, function()
    S.open_fire(tank)
    S.open_fire(carriers)
    S.open_fire(battery)
    battery:getController():setTask({
      id = 'FireAtPoint',
      params = { x = bx + 10, y = bz + 10, radius = 15, expendQty = 12, expendQtyEnabled = true },
    })
    return true
  end)
  once('ammo_lists', 3, function()
    local lists = {}
    for _, name in ipairs({ 'np-e38-f-1', 'np-e38-t1', 'np-e38-g1' }) do
      lists[name] = tracked[name].ammo0_rows
    end
    E.observe('ammo_rows', lists, { info = true })
    return true
  end)
  once('removals', 10, function()
    Unit.getByName('np-e38-d1'):destroy()
    Group.getByName('np-e38-e'):destroy()
    return true
  end)
  once('explosion', 5, function()
    trigger.action.explosion({ x = ax, y = land.getHeight({ x = ax, y = az - 6 }), z = az - 6 }, 30)
    return true
  end)
  if now >= next_sample then
    next_sample = now + STEP_S
    sample(now - started)
  end
  if now - started >= RUN_S then
    report()
    restore_debrief()
    S.clear()
    return 'done'
  end
  return nil
end, 400, 'running')
