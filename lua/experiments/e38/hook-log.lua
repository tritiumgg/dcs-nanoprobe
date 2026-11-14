-- E38: log the hook callbacks that may announce damage, and read the log.
--
-- ED's GUI script handles `onGameBdaEvent` (its "damage" event carries a
-- percent) and `onDebriefingEvent` (ED-CODE, MissionEditor/GameGUI.lua and
-- MissionEditor/modules/mul_bda.lua). Neither is in ED's API document. This
-- snippet asks DCS for both as user callbacks, and where the state holds
-- ED's own global of that name it wraps it, so the log shows which route
-- DCS uses. A second run returns the log and leaves the logger in place;
-- reset restores the globals and silences the callbacks. Run it in the hook
-- state and in the `gui` state: on a dedicated server they are two states.
-- states: hook gui

local E = ...
local NAMES = { 'onGameBdaEvent', 'onDebriefingEvent', 'onGameEvent' }
local LIMIT = 600

local function plain(value, depth)
  local kind = type(value)
  if kind == 'number' or kind == 'boolean' or kind == 'string' then
    return value
  end
  if kind == 'table' and depth < 3 then
    local copy = {}
    for key, inner in pairs(value) do
      copy[tostring(key)] = plain(inner, depth + 1)
    end
    return copy
  end
  return '<' .. kind .. '>'
end

local log = E.store.e38_log
if log then
  local out = { entries = log.entries, counts = log.counts, dropped = log.dropped, routes = log.routes }
  log.entries, log.counts, log.dropped = {}, {}, 0
  return out
end

log = { entries = {}, counts = {}, dropped = 0, routes = {}, on = true }
E.store.e38_log = log

local function record(route, name, ...)
  if not log.on then
    return
  end
  -- Counted by route, callback and kind: the event's name, or for a table
  -- its `type`.
  local first = (...)
  if type(first) == 'table' then
    first = first.typeName or first.type
  end
  local key = route .. ':' .. name .. ':' .. tostring(first)
  log.counts[key] = (log.counts[key] or 0) + 1
  if #log.entries >= LIMIT then
    log.dropped = log.dropped + 1
    return
  end
  local args = {}
  for i = 1, select('#', ...) do
    args[i] = plain((select(i, ...)), 0)
  end
  local ok, model = pcall(DCS.getModelTime)
  log.entries[#log.entries + 1] = { route = route, name = name, t = ok and model or -1, args = args }
end

local callbacks = {}
for _, name in ipairs(NAMES) do
  callbacks[name] = function(...)
    pcall(record, 'user', name, ...)
  end
  -- ED's own handler, when this state is the one that holds it.
  local original = rawget(_G, name)
  if type(original) == 'function' then
    log.routes[name] = 'global wrapped'
    rawset(_G, name, function(...)
      pcall(record, 'global', name, ...)
      return original(...)
    end)
    E.track.defer('e38 ' .. name, function()
      rawset(_G, name, original)
    end)
  else
    log.routes[name] = 'no global here'
  end
end
DCS.setUserCallbacks(callbacks)
E.track.defer('e38 log', function()
  log.on = false
end)
return 'logging'
