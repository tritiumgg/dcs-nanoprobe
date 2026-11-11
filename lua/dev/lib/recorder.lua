-- The event recorder, version 0: counts what each world event carries, by
-- event type and by cause. Load it with E.lib('recorder') in the `server`
-- state or the sandbox.
--
-- Development only. A snippet names the cause before it acts, with
-- R.mark('explosion'); every event that follows is filed under that name.
-- For each event type the recorder keeps how often each field was present
-- and what shape it had, and the first three events' plain values.
--
-- It never calls a method on an event or on an object in one: a method on an
-- event object can raise (LIVE). An object is recorded as `object`. A plain
-- table, such as `initiator_data`, is recorded by its keys.

local L = ...
local R = { cause = 'none', by = {}, order = {}, total = 0 }

local SAMPLES, ORDER = 3, 2000

local names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then
    names[id] = name
  end
end

local function plain(value)
  return type(value) == 'table' and getmetatable(value) == nil
end

-- A field's shape: its type, `object` for a table with a metatable, or the
-- sorted keys of a plain table.
local function shape(value)
  if type(value) ~= 'table' then
    return type(value)
  end
  if not plain(value) then
    return 'object'
  end
  local keys = {}
  for key in pairs(value) do
    keys[#keys + 1] = tostring(key)
  end
  table.sort(keys)
  return 'table{' .. table.concat(keys, ',') .. '}'
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
  local cause = R.by[R.cause]
  if not cause then
    cause = {}
    R.by[R.cause] = cause
  end
  local entry = cause[name]
  if not entry then
    entry = { id = id, count = 0, fields = {}, samples = {} }
    cause[name] = entry
  end
  entry.count = entry.count + 1
  for key, value in pairs(event) do
    local field = entry.fields[tostring(key)]
    if not field then
      field = {}
      entry.fields[tostring(key)] = field
    end
    local s = shape(value)
    field[s] = (field[s] or 0) + 1
  end
  if #entry.samples < SAMPLES then
    entry.samples[#entry.samples + 1] = sample(event)
  end
  if #R.order < ORDER then
    R.order[#R.order + 1] = { cause = R.cause, id = id }
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
  R.by, R.order, R.total, R.cause = {}, {}, 0, 'none'
end

return R
