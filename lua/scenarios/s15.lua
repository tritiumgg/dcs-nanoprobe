-- Helpers for the S15 experiments in lua/experiments/s15: spawns with the
-- fields the scenario library leaves out (ammunition, flares, chaff, a
-- country), a read of every value a capture keeps, and a text encoder, since
-- a snippet that crosses from the hook state gets text back. Load it with
-- E.lib('s15') in the `server` state.
--
-- Development only. Every group it adds is registered with the scenario
-- library, so reset removes it. Field names are those of a saved mission's
-- group and unit tables (ED-CODE: any saved mission's `mission` file).

local L = ...
local S = L.lib('scenario')
local X = {}

-- The call's value, or nil and the error text.
function X.try(fn, ...)
  local ok, value = pcall(fn, ...)
  if ok then
    return value
  end
  return nil, tostring(value)
end

local function is_array(t)
  local n = 0
  for _ in pairs(t) do
    n = n + 1
  end
  for i = 1, n do
    if t[i] == nil then
      return false
    end
  end
  return true
end

-- JSON-like text for a value: keys sorted, numbers to 6 digits, nothing
-- called on a function or a userdata.
function X.enc(v, depth)
  depth = depth or 0
  local kind = type(v)
  if kind == 'number' then
    if v ~= v then
      return '"nan"'
    end
    return v == math.floor(v) and string.format('%d', v) or string.format('%.6g', v)
  elseif kind == 'string' then
    return '"' .. v:gsub('[%c"\\]', function(c)
      return string.format('\\u%04x', c:byte())
    end) .. '"'
  elseif kind == 'boolean' or kind == 'nil' then
    return tostring(v)
  elseif kind == 'table' then
    if depth > 6 then
      return '"<depth>"'
    end
    local out = {}
    if is_array(v) then
      for i = 1, #v do
        out[i] = X.enc(v[i], depth + 1)
      end
      return '[' .. table.concat(out, ',') .. ']'
    end
    local keys = {}
    for k in pairs(v) do
      keys[#keys + 1] = k
    end
    table.sort(keys, function(a, b)
      return tostring(a) < tostring(b)
    end)
    for _, k in ipairs(keys) do
      out[#out + 1] = X.enc(tostring(k)) .. ':' .. X.enc(v[k], depth + 1)
    end
    return '{' .. table.concat(out, ',') .. '}'
  end
  return '"<' .. kind .. '>"'
end

-- The tail of a type name after its last dot: `weapons.shells.X` is `X`.
function X.tail(name)
  return tostring(name):match('([^.]+)$') or tostring(name)
end

local function waypoint(x, z, speed, alt)
  return S.air_point(x, z, speed, alt)
end

-- Add a ground, ship or helicopter-free group. `units` is a list of
-- { name, x, z, heading, ammo }; `o` holds `country` (default USA),
-- `category` (default GROUND), `route` (a list of { x, z, speed, action,
-- task }), `formation` (the first route point's formation, a string) and
-- `task`. Returns the group, or nil and the error text, and the seconds
-- coalition.addGroup took.
function X.add_ground(name, kind, units, o)
  o = o or {}
  local data = { name = name, task = o.task or 'Ground Nothing', visible = false, start_time = 0, units = {} }
  for i, u in ipairs(units) do
    data.units[i] = {
      name = u.name,
      type = kind,
      x = u.x,
      y = u.z,
      heading = u.heading or 0,
      skill = 'Average',
      playerCanDrive = false,
      ammo = u.ammo,
    }
  end
  data.x, data.y = data.units[1].x, data.units[1].y
  local points = {}
  for i, p in ipairs(o.route or { { x = units[1].x, z = units[1].z, speed = 0 } }) do
    local w = waypoint(p.x, p.z, p.speed)
    if p.action then
      w.action = p.action
    end
    if p.task then
      w.task = p.task
    end
    if i == 1 and o.formation then
      w.action = o.formation
    end
    points[i] = w
  end
  data.route = { points = points }
  local t0 = L.clock() or 0
  local group, err = X.try(coalition.addGroup, o.country or country.id.USA, o.category or Group.Category.GROUND, data)
  local cost = (L.clock() or 0) - t0
  S.groups[name] = true
  return group, err, cost
end

-- Add one aircraft or helicopter. `o` holds name, kind, category, country,
-- x, z, alt, speed, heading, fuel (kg), gun (percent), flare, chaff, pylons
-- (a mission file's `pylons` table), parking, task and points. The defaults
-- are an F-15C 5,000 m above the origin at 220 m/s heading 0 with 3,000 kg
-- of fuel and a route of three points ahead. Returns the group, or nil and
-- the error text.
function X.add_air(o)
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
        payload = {
          fuel = o.fuel or 3000,
          gun = o.gun or 50,
          flare = o.flare or 0,
          chaff = o.chaff or 0,
          pylons = o.pylons or {},
        },
        callsign = { 1, 1, 1, name = 'Enfield11' },
      },
    },
    route = { points = points },
  }
  local group, err = X.try(coalition.addGroup, o.country or country.id.USA, o.category or Group.Category.AIRPLANE, data)
  S.groups[name] = true
  return group, err
end

-- The ammunition rows getAmmo lists, each as { i, type, count, category,
-- display }, or nil when the unit has none.
function X.ammo_rows(unit)
  local rows = X.try(unit.getAmmo, unit)
  if type(rows) ~= 'table' then
    return nil
  end
  local out = {}
  for i, row in ipairs(rows) do
    local desc = type(row.desc) == 'table' and row.desc or {}
    out[i] = {
      i = i,
      type = desc.typeName,
      count = row.count,
      category = desc.category,
      display = desc.displayName,
    }
  end
  return out
end

-- Names of functions on the Unit, Object, Group, Controller and Airbase
-- classes that could read flares, chaff, pylons or a payload.
function X.candidate_methods()
  local found = {}
  local classes = { Unit = Unit, Object = Object, Group = Group, Controller = Controller, Airbase = Airbase }
  for class, t in pairs(classes) do
    pcall(function()
      for name, value in pairs(t) do
        if type(name) == 'string' and type(value) == 'function' then
          local lower = name:lower()
          for _, word in ipairs({ 'flare', 'chaff', 'pylon', 'payload', 'loadout', 'countermeasure', 'dispenser', 'store' }) do
            if lower:find(word, 1, true) then
              found[#found + 1] = class .. '.' .. name
              break
            end
          end
        end
      end
    end)
  end
  table.sort(found)
  return found
end

-- Every value a capture keeps, for a unit that exists, as a table. A read
-- that raises leaves its key out and puts the error in `errors`.
function X.capture(unit)
  local c, errors = {}, {}
  local function put(key, fn)
    local value, err = X.try(fn)
    if value ~= nil then
      c[key] = value
    elseif err then
      errors[key] = err
    end
  end
  put('name', function() return unit:getName() end)
  put('id', function() return unit:getObjectID() end)
  put('mission_id', function() return tonumber(unit:getID()) end)
  put('type', function() return unit:getTypeName() end)
  put('life', function() return unit:getLife() end)
  put('life0', function() return unit:getLife0() end)
  put('fuel', function() return unit:getFuel() end)
  put('fuel_max_kg', function() return unit:getDesc().fuelMassMax end)
  put('in_air', function() return unit:inAir() end)
  put('category', function() return unit:getDesc().category end)
  local point = X.try(unit.getPoint, unit)
  if point then
    c.x, c.y, c.z = point.x, point.y, point.z
  else
    errors.point = 'getPoint failed'
  end
  local pos = X.try(unit.getPosition, unit)
  if pos then
    c.heading = math.atan2(pos.x.z, pos.x.x)
    c.pitch = math.asin(math.max(-1, math.min(1, pos.x.y)))
    c.roll = math.atan2(-pos.z.y, pos.y.y)
  else
    errors.position = 'getPosition failed'
  end
  local v = X.try(unit.getVelocity, unit)
  if v then
    c.speed = math.sqrt(v.x ^ 2 + v.y ^ 2 + v.z ^ 2)
  end
  if c.fuel and c.fuel_max_kg then
    c.fuel_kg = c.fuel * c.fuel_max_kg
  end
  c.ammo = X.ammo_rows(unit)
  c.errors = errors
  return c
end

-- Keys of a table whose name matches one of the words, from a read that
-- may raise. Used on getDesc() to find where a payload could be named.
function X.desc_keys(unit)
  local desc = X.try(unit.getDesc, unit)
  if type(desc) ~= 'table' then
    return nil
  end
  local keys = {}
  for k in pairs(desc) do
    keys[#keys + 1] = tostring(k)
  end
  table.sort(keys)
  return keys
end

-- The unit's group destroyed, and the name free for another group.
function X.drop(name)
  S.destroy(name)
end

X.S = S
return X
