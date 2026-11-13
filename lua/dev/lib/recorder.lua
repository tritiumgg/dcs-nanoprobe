-- The event recorder, version 1: records what each world event carries, by
-- event type and by cause. Load it with E.lib('recorder') in the `server`
-- state or the sandbox.
--
-- Development only. A snippet names the cause before it acts, with
-- R.mark('explosion'); every event that follows is filed under that name.
-- For each event type the recorder keeps how often each field was present
-- and what shape it had, the largest distance between the event's time and
-- model time when it arrived (`skew`), and the first three events' plain
-- values. R.mark_unit files a unit's events under a cause of its own. A field
-- inside a plain table is recorded by its path, such as `pos.x`, down to
-- DEPTH levels. For the fields VALUES names, it counts each value too.
--
-- It never calls a method on an event or on an object in one: a method on an
-- event object can raise (LIVE). An object is a table with a metatable. It is
-- recorded as `object:<class>` when its metatable is one of the classes in
-- CLASSES, and as `object` otherwise, and its `id_` is read with rawget,
-- which calls no metamethod.

local L = ...
local R = { cause = 'none', by = {}, order = {}, total = 0, units = {} }

local SAMPLES, ORDER = 3, 2000

local names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then
    names[id] = name
  end
end

local DEPTH = 3
-- Fields whose values are counted too, by their last key: small sets of
-- numbers, such as DCS's Object.Category.
local VALUES = { object_category = true }
local CLASSES = { 'Unit', 'Weapon', 'StaticObject', 'Airbase', 'SceneryObject', 'Group', 'Object' }

local function plain(value)
  return type(value) == 'table' and getmetatable(value) == nil
end

-- The class an object's metatable names, by identity with the class tables.
local function class(value)
  local mt = getmetatable(value)
  for _, name in ipairs(CLASSES) do
    local c = rawget(_G, name)
    if c ~= nil and (mt == c or (type(mt) == 'table' and rawget(mt, '__index') == c)) then
      return name
    end
  end
  return nil
end

-- A value's shape: its type, `object` or `object:<class>` for a table with a
-- metatable, or `table` for a plain one.
local function shape(value)
  if type(value) ~= 'table' then
    return type(value)
  end
  if plain(value) then
    return 'table'
  end
  local c = class(value)
  return c and ('object:' .. c) or 'object'
end

-- Call `visit(path, shape)` for `value` at `path` and, inside a plain table,
-- for every field below it, and for the raw `id_` of an object.
local function walk(path, value, depth, visit)
  visit(path, shape(value), value)
  if type(value) ~= 'table' then
    return
  end
  if plain(value) then
    if depth >= DEPTH then
      return
    end
    for k, v in pairs(value) do
      walk(path .. '.' .. tostring(k), v, depth + 1, visit)
    end
  else
    local id = rawget(value, 'id_')
    if id ~= nil then
      visit(path .. '.id_', type(id))
    end
  end
end

-- The plain part of an event: scalars, and the scalars of plain tables.
local function sample(event)
  local out = {}
  for key, value in pairs(event) do
    if type(value) == 'number' or type(value) == 'string' or type(value) == 'boolean' then
      out[tostring(key)] = value
    elseif plain(value) then
      local inner = {}
      for k, v in pairs(value) do
        if type(v) == 'number' or type(v) == 'string' or type(v) == 'boolean' then
          inner[tostring(k)] = v
        end
      end
      out[tostring(key)] = inner
    end
  end
  return out
end

local function record(event)
  if type(event) ~= 'table' then
    return
  end
  R.total = R.total + 1
  local id = event.id
  local name = names[id] or ('event ' .. tostring(id))
  -- The cause: the one marked for the initiator's unit-name prefix, or the
  -- current one.
  local cause_name = R.cause
  local idata = rawget(event, 'initiator_data')
  local unit = type(idata) == 'table' and rawget(idata, 'unit_name')
  if type(unit) == 'string' then
    for _, u in ipairs(R.units) do
      if unit:sub(1, #u.prefix) == u.prefix then
        cause_name = u.cause
        break
      end
    end
  end
  local cause = R.by[cause_name]
  if not cause then
    cause = {}
    R.by[cause_name] = cause
  end
  local entry = cause[name]
  if not entry then
    entry = { id = id, count = 0, fields = {}, values = {}, samples = {} }
    cause[name] = entry
  end
  entry.count = entry.count + 1
  -- How far the event's time is from model time when it arrives.
  local t = rawget(event, 'time')
  local ok, now = pcall(function()
    return timer.getTime()
  end)
  if type(t) == 'number' and ok and type(now) == 'number' then
    local skew = math.abs(t - now)
    if not entry.skew or skew > entry.skew then
      entry.skew = skew
    end
  end
  local function visit(path, s, value)
    local field = entry.fields[path]
    if not field then
      field = {}
      entry.fields[path] = field
    end
    field[s] = (field[s] or 0) + 1
    if s == 'number' and VALUES[path:match('([^.]+)$')] then
      local values = entry.values[path]
      if not values then
        values = {}
        entry.values[path] = values
      end
      local key = tostring(value)
      values[key] = (values[key] or 0) + 1
    end
  end
  for key, value in pairs(event) do
    walk(tostring(key), value, 1, visit)
  end
  if #entry.samples < SAMPLES then
    entry.samples[#entry.samples + 1] = sample(event)
  end
  if #R.order < ORDER then
    R.order[#R.order + 1] = { cause = cause_name, id = id }
  end
end

-- Start recording. Safe to call again.
function R.start()
  if not R.started then
    R.started = true
    L.track.handler({
      onEvent = function(_, event)
        record(event)
      end,
    })
  end
end

-- File the events that follow under `cause`.
function R.mark(cause)
  R.cause = tostring(cause)
end

-- File every later event whose initiator's unit name starts with `prefix`
-- under `cause`, whatever R.mark says. For a scenario whose units act at
-- the same time.
function R.mark_unit(prefix, cause)
  R.units[#R.units + 1] = { prefix = tostring(prefix), cause = tostring(cause) }
end

-- How many events of type `name` arrived under `cause`.
function R.count(cause, name)
  local entry = R.by[cause] and R.by[cause][name]
  return entry and entry.count or 0
end

-- How many events of any type arrived under `cause`.
function R.count_all(cause)
  local n = 0
  for _, entry in pairs(R.by[cause] or {}) do
    n = n + entry.count
  end
  return n
end

-- The event IDs that arrived under `cause`, in order.
function R.ids(cause)
  local out = {}
  for _, item in ipairs(R.order) do
    if item.cause == cause then
      out[#out + 1] = item.id
    end
  end
  return out
end

-- The sorted keys of field `field` in the first event of type `name` under
-- `cause`, when that field is a plain table.
function R.keys(cause, name, field)
  local entry = R.by[cause] and R.by[cause][name]
  local value = entry and entry.samples[1] and entry.samples[1][field]
  if type(value) ~= 'table' then
    return nil
  end
  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = key
  end
  table.sort(keys)
  return keys
end

-- Everything recorded: by cause, then by event type.
function R.report()
  return { total = R.total, by = R.by }
end

function R.clear()
  R.by, R.order, R.total, R.cause, R.units = {}, {}, 0, 'none', {}
end

return R
