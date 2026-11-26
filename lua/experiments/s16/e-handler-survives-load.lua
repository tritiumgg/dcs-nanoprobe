-- Does an event handler an eval snippet installed survive `cargo xtask load`?
-- Rule: run it twice, with a load between. The snippet runs in the hook state,
-- which keeps what it knows across a load, and installs and reads the handler in
-- the server state through E.cross.
--
-- First run: installs a counting handler and a global that holds its count in
-- the server state, makes the world send two events (a mark added and
-- removed), and reports that the handler counted them.
--
-- Second run, after the load: if the server state's global is gone, the load
-- gave it a new Lua state and the handler went with it (`state_reset`). If it
-- is there, the snippet makes two more events and reports whether the count rose
-- (`handler_survived`). It then removes the handler and the global, so the run
-- that finds the handler is also the one that cleans up. A first run that is
-- never followed by a second is cleaned up by `cargo xtask reset`.
--
--   cargo xtask eval <target> hook lua/experiments/s16/e-handler-survives-load.lua
--   cargo xtask load <target> <miz>
--   cargo xtask eval <target> hook lua/experiments/s16/e-handler-survives-load.lua
-- states: hook
-- timeout: 60

local E = ...
local MARKER = '__np_s16_survive_marker'

local function cross(source)
  local ok, value = E.cross('server', source)
  if not ok then
    error('server: ' .. tostring(value))
  end
  return value
end

-- Make two events, and give the world a moment before the caller reads. DCS
-- ignores an add under an ID removed earlier in the process (LIVE), so each run
-- takes a new ID, from 500,000,000 plus the hook's real time in tenths of a second.
-- That stays below 900,000,000 (the drawing-layers experiments' base, and far
-- below the platform pool, 2^30 to 2^31 - 1) for 4.0e7 s, about 463 days.
local function events()
  local id = 500000000 + math.floor(DCS.getRealTime() * 10)
  return string.format([[
local o = E.lib('scenario').origin()
trigger.action.markToAll(%d, 'np-s16-survive', { x = o.x, y = 0, z = o.z }, true)
trigger.action.removeMark(%d)
return 'events'
]], id, id)
end

local INSTALL = [[
local count = { count = 0, dead = false }
count.wrapper = E.track.handler({ onEvent = function(_, event)
  if not count.dead then
    count.count = count.count + 1
  end
end })
E.track.global('__np_s16_survive', count)
return 'installed'
]]

local READ = [[
local held = rawget(_G, '__np_s16_survive')
if type(held) ~= 'table' then
  return 'none'
end
return tostring(held.count)
]]

local REMOVE = [[
local held = rawget(_G, '__np_s16_survive')
if type(held) ~= 'table' then
  return 'none'
end
held.dead = true
world.removeEventHandler(held.wrapper)
rawset(_G, '__np_s16_survive', nil)
return 'removed'
]]

-- The wait after making events: a second of the hook's real time.
local function after(seconds, fn)
  local at = DCS.getRealTime() + seconds
  return E.wait(function()
    if DCS.getRealTime() < at then
      return nil
    end
    return fn()
  end, seconds + 30, 'running')
end

local first = rawget(_G, MARKER)
if not first then
  E.observe('install', cross(INSTALL), { info = true })
  cross(events())
  return after(1, function()
    local counted = tonumber(cross(READ))
    rawset(_G, MARKER, { count = counted })
    E.track.defer('survive marker', function()
      rawset(_G, MARKER, nil)
    end)
    E.observe('events_counted', counted, { info = true })
    return 'installed; load the mission and run this again'
  end)
end

-- Second run.
local held = cross(READ)
if held == 'none' then
  rawset(_G, MARKER, nil)
  E.observe('state_reset', true, { info = true })
  E.observe('handler_survived', false, { info = true })
  E.observe('count_before', first.count, { info = true })
  return 'the load gave the server state a new Lua state'
end
local before = tonumber(held)
cross(events())
return after(1, function()
  local after_count = tonumber(cross(READ))
  E.observe('state_reset', false, { info = true })
  E.observe('count_before', before, { info = true })
  E.observe('count_after', after_count, { info = true })
  E.observe('handler_survived', after_count > before, { info = true })
  E.observe('removed', cross(REMOVE), { info = true })
  rawset(_G, MARKER, nil)
  return 'done'
end)
