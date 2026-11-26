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

-- The center every scenario is laid out around: the first airbase's point.
function S.origin()
  if not S.center then
    local base = world.getAirbases()[1]
    local point = base:getPoint()
    S.center = { x = point.x, y = point.y, z = point.z }
  end
  return S.center
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
-- `category` is a Group.Category, GROUND by default; SHIP adds ships.
-- Returns the group and the seconds coalition.addGroup took.
function S.ground(name, kind, units, route, country, category)
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
  local group = coalition.addGroup(country or country_id('USA'), category or Group.Category.GROUND, data)
  local cost = now() - t0
  S.groups[name] = true
  return group, cost
end

-- Add `n` units of `kind` in groups of `per`, on a square grid `spacing`
-- meters apart, `offset` meters north of the origin. `label` keeps the names
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
-- heading north from `offset` meters east of the origin. Returns the group
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

-- One aircraft in the air. `o` holds x, z, alt, speed, heading, fuel (kg),
-- gun (percent) and pylons (a mission file's `pylons` table); the defaults
-- are 5,000 m above the origin, 220 m/s, heading north, 3,000 kg, 50 % and
-- empty pylons. The route is three points ahead of it,
-- or `o.points` when given. `o.kind`, `o.category` (a Group.Category),
-- `o.country` (a country ID) and `o.task` default to an F-15C of the USA
-- on CAP. Returns the group and the seconds coalition.addGroup took.
function S.spawn_aircraft(o)
  o = o or {}
  local origin = S.origin()
  local name = o.name or 'np-air'
  local x, z = o.x or origin.x, o.z or (origin.z + 3000)
  local alt, speed, heading = o.alt or 5000, o.speed or 220, o.heading or 0
  local points = o.points
  if not points then
    points = {}
    for i = 0, 2 do
      points[#points + 1] = waypoint(x + math.cos(heading) * i * 40000, z + math.sin(heading) * i * 40000, speed, alt)
    end
  end
  local data = {
    name = name,
    task = o.task or 'CAP',
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
        parking = o.parking,
        payload = { fuel = o.fuel or 3000, gun = o.gun or 50, flare = 0, chaff = 0, pylons = o.pylons or {} },
        callsign = { 1, 1, 1, name = 'Enfield11' },
      },
    },
    route = { points = points },
  }
  local t0 = now()
  local group = coalition.addGroup(o.country or country_id('USA'), o.category or Group.Category.AIRPLANE, data)
  local cost = now() - t0
  S.groups[name] = true
  return group, cost
end

-- The first airdrome the mission has, as its ID, its point and the object,
-- or nil when it has none.
function S.airdrome()
  for _, base in ipairs(world.getAirbases()) do
    local desc = base:getDesc()
    if desc and desc.category == Airbase.Category.AIRDROME then
      local p = base:getPoint()
      return { id = base:getID(), x = p.x, y = p.y, z = p.z, base = base }
    end
  end
  return nil
end

-- A route point for an aircraft at an airdrome: `kind` is `runway` (take
-- off from the runway), `hot` (from parking, engines running), `cold` (from
-- parking, engines off) or `land`. The field names are those of a saved
-- mission's route points (ED-CODE).
function S.airdrome_point(kind, base, speed, alt)
  local types = {
    runway = { 'TakeOff', 'From Runway' },
    hot = { 'TakeOffParkingHot', 'From Parking Area Hot' },
    cold = { 'TakeOffParking', 'From Parking Area' },
    land = { 'Land', 'Landing' },
  }
  local t = assert(types[kind], 'airdrome_point: unknown kind')
  local p = waypoint(base.x, base.z, speed or 0, alt or 0)
  p.type, p.action, p.airdromeId = t[1], t[2], base.id
  p.alt_type = 'RADIO'
  return p
end

-- A plain turning point for an aircraft.
function S.air_point(x, z, speed, alt)
  return waypoint(x, z, speed, alt)
end

-- Order a group to open fire: ROE option 0, value 2 for a ground group
-- (ED-CODE, Scripts/UI/RadioCommandDialogPanel/Config/groundUnit.lua).
-- Returns whether the call went through.
function S.open_fire(group)
  return (pcall(function()
    group:getController():setOption(0, 2)
  end))
end

-- A static object `east` meters east and `north` meters north of the
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
  S.lanes, S.ship_places, S.air_places, S.static_places = 0, 0, 0, 0
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


-- The moving, mixed scenario of step S10. Of `n` objects: 92 % ground units
-- in moving groups of 10, 4 % ships in moving pairs on water, 2 % aircraft
-- (half helicopters, half airplanes, one to a group), 2 % statics. The
-- shares keep aircraft few, since each costs the server a frame of its own
-- to spawn and more to fly. With no water within 60 km of the origin the
-- ships' share goes to ground units, and the result says so. `o.label` keeps
-- two scenarios' names apart; `o.speed` is the ground speed in m/s
-- (default 6); `o.moving` is the share of ground groups that move (default
-- 1, all of them). Returns a summary: counts by kind, the group names, and the
-- seconds the spawns took.
S.MIX = { ground = 0.92, ships = 0.04, air = 0.02, statics = 0.02 }

-- A point on water within 60 km of the origin, or nil: the first of eight
-- directions and 2 km steps where land.getSurfaceType says WATER (3).
function S.water()
  local origin = S.origin()
  for r = 2000, 60000, 2000 do
    for i = 0, 7 do
      local a = i * math.pi / 4
      local p = { x = origin.x + math.cos(a) * r, y = origin.z + math.sin(a) * r }
      local ok, t = pcall(land.getSurfaceType, p)
      if ok and t == 3 then
        return { x = p.x, z = p.y, angle = a }
      end
    end
  end
  return nil
end

function S.spawn_mixed(n, o)
  o = o or {}
  local label = o.label or 'mix'
  local origin = S.origin()
  local speed = o.speed or 6
  local t0 = now()
  local out = { groups = {}, ground = 0, ships = 0, air = 0, statics = 0 }
  local ships = math.floor(n * S.MIX.ships / 2) * 2
  local water = ships > 0 and S.water() or nil
  if not water then
    ships = 0
    out.no_water = true
  end
  local air = math.floor(n * S.MIX.air)
  local statics = math.floor(n * S.MIX.statics)
  local ground = n - ships - air - statics
  -- Ground: groups of 10 in columns, each in a lane of its own, so no group
  -- drives into another: lanes 120 m apart from 2 km east of the origin, 92
  -- to a block, blocks 6 km apart along the direction of travel, every group
  -- driving 12 km north at the same speed. Lanes are numbered across spawns,
  -- so a later spawn never starts on top of an earlier one. (A layout with
  -- 20 groups to a line drove each group into the one ahead and brought the
  -- server below 1 frame a second at 2,000 units: LIVE, S10.)
  local g = 0
  while out.ground < ground do
    g = g + 1
    local count = math.min(10, ground - out.ground)
    local lane = S.lanes or 0
    S.lanes = lane + 1
    local x0 = origin.x - math.floor(lane / 92) * 6000
    local z0 = origin.z + 2000 + (lane % 92) * 120
    local units = {}
    for i = 1, count do
      units[i] = { name = string.format('np-%s-u%d-%d', label, g, i), x = x0 - (i - 1) * 15, z = z0 }
    end
    -- With `o.moving` below 1, only that share of the groups get a route;
    -- the rest stay where they are, alive.
    local moves = (o.moving or 1) >= 1 or lane % math.floor(1 / o.moving + 0.5) == 0
    local route
    if moves then
      route = {}
      for k = 0, 3 do
        route[#route + 1] = { x = x0 + k * 4000, z = z0, speed = speed }
      end
    end
    local name = string.format('np-%s-g%d', label, g)
    S.ground(name, o.ground_kind or 'M-113', units, route)
    out.groups[#out.groups + 1] = name
    out.ground = out.ground + count
  end
  -- Ships: pairs moving out from the water point, 300 m apart.
  for k = 1, ships / 2 do
    -- Places are numbered across spawns, as the lanes are.
    local place = (S.ship_places or 0) + 1
    S.ship_places = place
    local x0 = water.x + math.cos(water.angle) * place * 300
    local z0 = water.z + math.sin(water.angle) * place * 300
    local units = {
      { name = string.format('np-%s-s%d-1', label, k), x = x0, z = z0 },
      { name = string.format('np-%s-s%d-2', label, k), x = x0 + 60, z = z0 },
    }
    local route = {}
    for j = 0, 2 do
      route[#route + 1] = {
        x = x0 + math.cos(water.angle) * j * 5000,
        z = z0 + math.sin(water.angle) * j * 5000,
        speed = 8,
      }
    end
    local name = string.format('np-%s-sg%d', label, k)
    local group = S.ground(name, o.ship_kind or 'speedboat', units, route, nil, Group.Category.SHIP)
    if group then
      out.groups[#out.groups + 1] = name
      out.ships = out.ships + 2
    end
  end
  -- Aircraft: one to a group, 3 km apart, half of them helicopters.
  for k = 1, air do
    local heli = k % 2 == 0
    local name = string.format('np-%s-a%d', label, k)
    local place = (S.air_places or 0) + 1
    S.air_places = place
    S.spawn_aircraft({
      name = name,
      kind = heli and 'UH-1H' or 'F-15C',
      category = heli and Group.Category.HELICOPTER or Group.Category.AIRPLANE,
      x = origin.x - 5000 - place * 3000,
      z = origin.z,
      alt = heli and 500 or 6000,
      speed = heli and 50 or 200,
      task = heli and 'Transport' or 'CAP',
      fuel = heli and 300 or 3000,
    })
    out.groups[#out.groups + 1] = name
    out.air = out.air + 1
  end
  -- Statics: a row 30 m apart, 1 km west of the origin.
  for k = 1, statics do
    local name = string.format('np-%s-st%d', label, k)
    local place = (S.static_places or 0) + 1
    S.static_places = place
    -- Every second static is cargo, which can move: `ammo_cargo` of ED's
    -- Scripts/Database/db_countries.lua (ED-CODE).
    if k % 2 == 0 then
      S.spawn_static(name, -1000, place * 30, 'ammo_cargo', 'Cargos')
      out.cargo = (out.cargo or 0) + 1
    else
      S.spawn_static(name, -1000, place * 30)
    end
    out.statics = out.statics + 1
  end
  out.seconds = now() - t0
  return out
end

-- Churn: an explosion at each of `count` units of the scenario's ground
-- groups, which kills them and the scenery near them, sending death events.
-- Returns the names of the units it aimed at.
function S.churn(groups, count)
  local hit = {}
  for _, name in ipairs(groups) do
    if #hit >= count then
      break
    end
    local group = Group.getByName(name)
    local units = group and group:getUnits() or {}
    local unit = units[#units]
    if unit and unit:isExist() then
      local p = unit:getPoint()
      trigger.action.explosion(p, 300)
      hit[#hit + 1] = unit:getName()
    end
  end
  return hit
end

return S
