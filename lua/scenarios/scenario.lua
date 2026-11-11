-- The scenario library, version 0: spawn and remove test objects from a
-- snippet in the `server` state. Load it with E.lib('scenario').
--
-- Development only. Every group and static it adds has a name starting with
-- `np-`, and all of them are destroyed at reset. Positions are relative to
-- the first airbase the mission has, so a scenario runs on any map.
--
-- The group tables hold the fields DCS's own mission files use for a ground
-- group and an aircraft group (ED-CODE: any saved mission's `mission` file).

local L = ...
local S = { groups = {}, statics = {} }

local function now()
  return L.clock() or 0
end

-- The centre every scenario is laid out around: the first airbase's point.
function S.origin()
  if not S.centre then
    local base = world.getAirbases()[1]
    local point = base:getPoint()
    S.centre = { x = point.x, y = point.y, z = point.z }
  end
  return S.centre
end

local function country_id(name)
  return country.id[name]
end

local function waypoint(x, z, speed, alt)
  return {
    x = x,
    y = z,
    alt = alt,
    alt_type = alt and 'BARO' or nil,
    type = 'Turning Point',
    action = alt and 'Turning Point' or 'Off Road',
    speed = speed,
    speed_locked = true,
    ETA = 0,
    ETA_locked = false,
    formation_template = '',
    task = { id = 'ComboTask', params = { tasks = {} } },
  }
end

-- Add a ground group. `units` is a list of { name, x, z, heading }; `route`
-- a list of { x, z, speed }, or nil for a group that stays where it is.
-- Returns the group and the seconds coalition.addGroup took.
function S.ground(name, kind, units, route, country)
  local data = { name = name, task = 'Ground Nothing', visible = false, start_time = 0, units = {} }
  for i, u in ipairs(units) do
    data.units[i] = {
      name = u.name,
      type = kind,
      x = u.x,
      y = u.z,
      heading = u.heading or 0,
      skill = 'Average',
      playerCanDrive = false,
    }
  end
  data.x, data.y = data.units[1].x, data.units[1].y
  local points = {}
  for i, p in ipairs(route or { { x = units[1].x, z = units[1].z, speed = 0 } }) do
    points[i] = waypoint(p.x, p.z, p.speed)
  end
  data.route = { points = points }
  local t0 = now()
  local group = coalition.addGroup(country or country_id('USA'), Group.Category.GROUND, data)
  local cost = now() - t0
  S.groups[name] = true
  return group, cost
end

-- Add `n` units of `kind` in groups of `per`, on a square grid `spacing`
-- metres apart, `offset` metres north of the origin. `label` keeps the names
-- of two grids apart. Returns the group names and each group's cost in
-- seconds.
function S.spawn_grid(n, options)
  options = options or {}
  local kind, per, spacing = options.kind or 'M-113', options.per or 10, options.spacing or 20
  local label, start = options.label or 'grid', options.start or 0
  local origin = S.origin()
  local side = math.ceil(math.sqrt(start + n))
  local names, costs = {}, {}
  local index = start
  while index < start + n do
    local units = {}
    for _ = 1, math.min(per, start + n - index) do
      units[#units + 1] = {
        name = string.format('np-%s-%d', label, index + 1),
        x = origin.x + (options.offset or 0) + (index % side) * spacing,
        z = origin.z + math.floor(index / side) * spacing,
      }
      index = index + 1
    end
    local name = string.format('np-%s-g%d', label, #names + 1 + math.floor(start / per))
    local _, cost = S.ground(name, kind, units)
    names[#names + 1] = name
    costs[#costs + 1] = cost
  end
  return names, costs
end

-- A convoy of `count` vehicles with a route of four points, 500 m apart,
-- heading north from `offset` metres east of the origin. Returns the group
-- name and the route.
function S.spawn_convoy(options)
  options = options or {}
  local origin = S.origin()
  local name = options.name or 'np-convoy'
  local speed = options.speed or 8
  local z = origin.z + (options.offset or 600)
  local route = {}
  for i = 0, 3 do
    route[#route + 1] = { x = origin.x + i * 500, z = z, speed = speed }
  end
  local units = {}
  for i = 1, options.count or 4 do
    units[i] = { name = name .. '-' .. i, x = origin.x - (i - 1) * 20, z = z }
  end
  S.ground(name, options.kind or 'M-113', units, route)
  return name, route
end

-- One aircraft in the air. `o` holds x, z, alt, speed, heading, fuel (kg)
-- and gun (percent); the defaults are 5,000 m above the origin, 220 m/s,
-- heading north, 3,000 kg and 50 %. The route is three points ahead of it.
-- Returns the group and the seconds coalition.addGroup took.
function S.spawn_aircraft(o)
  o = o or {}
  local origin = S.origin()
  local name = o.name or 'np-air'
  local x, z = o.x or origin.x, o.z or (origin.z + 3000)
  local alt, speed, heading = o.alt or 5000, o.speed or 220, o.heading or 0
  local points = {}
  for i = 0, 2 do
    points[#points + 1] = waypoint(x + math.cos(heading) * i * 40000, z + math.sin(heading) * i * 40000, speed, alt)
  end
  local data = {
    name = name,
    task = 'CAP',
    start_time = 0,
    x = x,
    y = z,
    units = {
      {
        name = name .. '-1',
        type = o.kind or 'F-15C',
        x = x,
        y = z,
        alt = alt,
        alt_type = 'BARO',
        speed = speed,
        heading = heading,
        skill = 'High',
        payload = { fuel = o.fuel or 3000, gun = o.gun or 50, flare = 0, chaff = 0, pylons = {} },
        callsign = { 1, 1, 1, name = 'Enfield11' },
      },
    },
    route = { points = points },
  }
  local t0 = now()
  local group = coalition.addGroup(country_id('USA'), Group.Category.AIRPLANE, data)
  local cost = now() - t0
  S.groups[name] = true
  return group, cost
end

-- A static object `east` metres east and `north` metres north of the
-- origin. Returns the object and the seconds the call took.
function S.spawn_static(name, north, east, kind, category)
  local origin = S.origin()
  local t0 = now()
  local static = coalition.addStaticObject(country_id('USA'), {
    name = name,
    type = kind or 'Windsock',
    category = category or 'Fortifications',
    x = origin.x + north,
    y = origin.z + east,
    heading = 0,
  })
  local cost = now() - t0
  S.statics[name] = true
  return static, cost
end

-- Kill a unit with an explosion at its position.
function S.kill(unit_name, power)
  local unit = Unit.getByName(unit_name)
  if not unit then
    error('scenario.kill: no unit ' .. tostring(unit_name), 2)
  end
  trigger.action.explosion(unit:getPoint(), power or 100)
end

-- Destroy one group this library added.
function S.destroy(name)
  local group = Group.getByName(name)
  if group then
    group:destroy()
  end
  S.groups[name] = nil
end

-- Destroy everything this library added. Returns how many groups and
-- statics it destroyed.
function S.clear()
  local groups, statics = 0, 0
  for name in pairs(S.groups) do
    local group = Group.getByName(name)
    if group then
      group:destroy()
      groups = groups + 1
    end
  end
  for name in pairs(S.statics) do
    local static = StaticObject.getByName(name)
    if static then
      static:destroy()
      statics = statics + 1
    end
  end
  S.groups, S.statics = {}, {}
  return groups, statics
end

-- How many of this library's groups still exist.
function S.remaining()
  local n = 0
  for name in pairs(S.groups) do
    if Group.getByName(name) then
      n = n + 1
    end
  end
  return n
end

L.track.defer('scenario', S.clear)

return S
