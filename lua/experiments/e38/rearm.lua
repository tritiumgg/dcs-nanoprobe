-- E38: when does an AI ground unit rearm, and does anything announce it?
--
-- Eight tanks 1.5 km apart each fire at a point on the ground and then
-- stand. Each has a different neighbor:
--   1  none within 1.4 km;
--   2  an M 818 truck 25 m away (the type ED's database shows under a
--      display name; trucks.lua lists both);
--   3  an M978 HEMTT Tanker, a fuel truck, 25 m away;
--   4  a Ural-375 truck of the tank's own coalition 25 m away;
--   5, 6, 7  an M 818 at 100 m, 200 m and 400 m;
--   8  an M 818 of the other coalition 25 m away, added after the firing,
--      with the tank ordered to hold its fire.
-- Once a second of model time the snippet reads each tank's getAmmo, row by
-- row, and each truck's life, fuel and getAmmo, and logs every change. Every
-- world event that names a tank or a truck is counted. The report gives, for
-- each tank, what it fired, when the count first rose again, each step, and
-- whether it came back to full.
-- states: server
-- timeout: 480

local E = ...
local S = E.lib('scenario')
local RUN_S = 420
local FIRE_AT_S, LATE_TRUCK_S = 2, 90

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

-- kind of truck, metres from the tank, and whether it is the other side's.
local NEIGHBORS = {
  { label = 'none' },
  { label = 'M 818 at 25 m', kind = 'M 818', metres = 25 },
  { label = 'M978 HEMTT Tanker at 25 m', kind = 'M978 HEMTT Tanker', metres = 25 },
  { label = 'Ural-375 at 25 m', kind = 'Ural-375', metres = 25 },
  { label = 'M 818 at 100 m', kind = 'M 818', metres = 100 },
  { label = 'M 818 at 200 m', kind = 'M 818', metres = 200 },
  { label = 'M 818 at 400 m', kind = 'M 818', metres = 400 },
  { label = 'M 818 of the other coalition at 25 m, added later', kind = 'M 818', metres = 25, other = true },
}

-- Eight sites on land, 1.5 km apart, each with 700 m of land to the north.
local origin = S.origin()
local sites = {}
for row = 0, 5 do
  for column = 0, 9 do
    local x, z = origin.x + 3000 + row * 1500, origin.z + 6000 + column * 1500
    local on_land = true
    for _, north in ipairs({ 0, 350, 700 }) do
      local kind = try(function()
        return land.getSurfaceType({ x = x + north, y = z })
      end)
      on_land = on_land and (kind == nil or kind == 1)
    end
    if on_land and #sites < #NEIGHBORS then
      sites[#sites + 1] = { x = x, z = z }
    end
  end
end
assert(#sites == #NEIGHBORS, 'not enough land for the probe')

local function rows(handle)
  local list, total = {}, 0
  local ammo = try(Unit.getAmmo, handle)
  if type(ammo) == 'table' then
    for _, entry in ipairs(ammo) do
      local name = type(entry.desc) == 'table' and tostring(entry.desc.typeName) or '?'
      local count = tonumber(entry.count) or 0
      list[name] = (list[name] or 0) + count
      total = total + count
    end
  end
  return list, total
end

local tanks = {}
for i, neighbor in ipairs(NEIGHBORS) do
  local site = sites[i]
  local name = 'np-e38-m' .. i
  local group = S.ground(name, 'M-1 Abrams', { { name = name .. '-1', x = site.x, z = site.z, heading = 0 } })
  local tank = { label = neighbor.label, name = name .. '-1', group = group, site = site, neighbor = neighbor, changes = {} }
  if neighbor.kind and not neighbor.other then
    S.ground('np-e38-n' .. i, neighbor.kind, { { name = 'np-e38-n' .. i .. '-1', x = site.x - neighbor.metres, z = site.z, heading = 0 } })
    tank.truck_name = 'np-e38-n' .. i .. '-1'
  end
  tanks[i] = tank
end

local function truck_state(handle)
  local _, total = rows(handle)
  return string.format('life %s fuel %s ammo %d', tostring(try(Unit.getLife, handle)), tostring(try(Unit.getFuel, handle)), total)
end

local function sample(now)
  for _, tank in ipairs(tanks) do
    if not tank.handle then
      local unit = try(Unit.getByName, tank.name)
      if unit then
        tank.id = rawget(unit, 'id_')
        tank.handle = { id_ = tank.id }
        tank.rows0, tank.total0 = rows(tank.handle)
        tank.total, tank.low = tank.total0, tank.total0
      end
    else
      local _, total = rows(tank.handle)
      if total ~= tank.total then
        if total < tank.total then
          tank.last_fall, tank.low = now, math.min(tank.low, total)
        else
          tank.first_rise = tank.first_rise or now
          if #tank.changes < 40 then
            tank.changes[#tank.changes + 1] = string.format('%d:+%d', math.floor(now + 0.5), total - tank.total)
          end
          if total >= tank.total0 and not tank.full_at then
            tank.full_at = now
          end
        end
        tank.total = total
      end
    end
    if tank.truck_name and not tank.truck then
      local unit = try(Unit.getByName, tank.truck_name)
      if unit then
        tank.truck_id = rawget(unit, 'id_')
        tank.truck = { id_ = tank.truck_id }
        tank.truck_first = truck_state(tank.truck)
        tank.truck_last, tank.truck_changes = tank.truck_first, 0
      end
    elseif tank.truck then
      local state = truck_state(tank.truck)
      if state ~= tank.truck_last then
        tank.truck_last, tank.truck_changes = state, tank.truck_changes + 1
      end
    end
  end
end

local function report()
  for i, tank in ipairs(tanks) do
    local rows_now = tank.handle and rows(tank.handle) or {}
    local by_row = {}
    for name, count in pairs(tank.rows0 or {}) do
      by_row[name] = string.format('%d > %d', count, rows_now[name] or -1)
    end
    local named = {}
    for _, event in ipairs(events) do
      local who = (event.initiator == tank.id and 'tank') or (event.target == tank.id and 'tank as target')
        or (tank.truck_id and event.initiator == tank.truck_id and 'truck')
        or (tank.truck_id and event.target == tank.truck_id and 'truck as target') or nil
      if who then
        local key = who .. ' ' .. event.name
        named[key] = (named[key] or 0) + 1
      end
    end
    E.observe(string.format('%d %s', i, tank.label), {
      start = tank.total0, lowest = tank.low, at_end = tank.total, last_fall_s = tank.last_fall,
      first_rise_s = tank.first_rise, full_at_s = tank.full_at, rises = tank.changes, rows = by_row,
      truck_first = tank.truck_first, truck_last = tank.truck_last, truck_changes = tank.truck_changes, events = named,
    }, { info = true })
  end
end

local done = {}
local function once(name, at, fn)
  if not done[name] and timer.getTime() - started >= at then
    done[name] = true
    E.observe(name, try(fn) or 'failed or nothing returned', { info = true })
  end
end

local next_sample = started
return E.wait(function()
  local now = timer.getTime()
  once('fire', FIRE_AT_S, function()
    for _, tank in ipairs(tanks) do
      S.open_fire(tank.group)
      tank.group:getController():setTask({
        id = 'FireAtPoint',
        params = { x = tank.site.x + 600, y = tank.site.z, radius = 20, expendQty = 8, expendQtyEnabled = true },
      })
    end
    return 'ordered'
  end)
  once('late_truck', LATE_TRUCK_S, function()
    for i, tank in ipairs(tanks) do
      if tank.neighbor.other then
        -- ROE option 0, value 4: weapon hold (ED-CODE, as S.open_fire).
        tank.group:getController():setOption(0, 4)
        S.ground('np-e38-n' .. i, tank.neighbor.kind, {
          { name = 'np-e38-n' .. i .. '-1', x = tank.site.x - tank.neighbor.metres, z = tank.site.z, heading = 0 },
        }, nil, country.id.RUSSIA)
        tank.truck_name = 'np-e38-n' .. i .. '-1'
      end
    end
    return 'added'
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
end, 460, 'running')
