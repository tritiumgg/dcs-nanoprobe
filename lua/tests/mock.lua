-- The strict mock DCS host raises on unknown and unmocked names and
-- reproduces the quirks its header lists.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local host = mock.new({ writedir = '/nowhere/' })
local gui, server = host.states.gui, host.states.server

-- Strictness.
S.raises('gui DCS.noSuchCall is not in the listing', function()
  return gui.DCS.noSuchCall()
end)
S.raises('gui DCS.exportToMiz is in the DCS API but not mocked', function()
  return gui.DCS.exportToMiz()
end)
S.raises('server timer.bogus is not in the listing', function()
  return server.timer.bogus
end)
assert(gui.Unit == nil and gui.world == nil, 'the gui state has no scripting API')
assert(server.net == nil, 'the server state has no net')
S.ok('an unknown name raises, a listed but unmocked one raises differently')

-- net.dostring_in.
local dostring_in = gui.net.dostring_in
assert(dostring_in('server', 'x = 41; return x + 1') == '42')
assert(server.x == 41 and gui.x == nil and rawget(_G, 'x') == nil,
  'a global set by a chunk stays in its state')
assert(dostring_in('scripting', 'return x') == '41', 'scripting is server')
assert(dostring_in('server', 'error("boom", 0)') == 'boom')
assert(dostring_in('server', 'return (') :find('expected', 1, true))
assert(dostring_in('userhooks', 'return 1') == nil)
S.raises('takes a state name and a string, and no more', dostring_in, 'server', 'return 1', 2)
S.raises('the mission state is not mocked', dostring_in, 'mission', 'return 1')
S.ok('dostring_in: own globals per state, errors as strings, nil for unknown names')

-- Callbacks, lifecycle and timers.
local seen = {}
local function record(name)
  return function()
    seen[#seen + 1] = name
  end
end
gui.DCS.setUserCallbacks({ onSimulationFrame = record('frame A') })
gui.DCS.setUserCallbacks({
  onSimulationFrame = record('frame B'),
  onMissionLoadBegin = record('load begin'),
  onSimulationStart = record('start'),
  onSimulationPause = record('pause'),
  onSimulationResume = record('resume'),
})
host:load_mission('first', { paused = true })
assert(table.concat(seen, ',') == 'load begin,start', table.concat(seen, ','))
assert(gui.DCS.getPause() == true, 'started paused')
S.ok('a mission can start paused with no pause callback')

seen = {}
local ticks = 0
server.timer.scheduleFunction(function(_, t)
  ticks = ticks + 1
  return t + 0.0001
end, nil, 0)
host:frame(1 / 60)
host:frame(1 / 60)
assert(ticks == 0 and server.timer.getTime() == 0, 'a timer ran while paused')
assert(table.concat(seen, ',') == 'frame A,frame B,frame A,frame B', table.concat(seen, ','))
S.ok('while paused: frames arrive, timers wait, model time stands still')

host:set_pause(false, { silent = true })
for _ = 1, 5 do
  host:frame(1 / 60)
end
assert(ticks == 5, 'a per-frame timer ran ' .. ticks .. ' times in 5 frames')
assert(math.abs(server.timer.getTime() - 5 / 60) < 1e-9)
S.ok('a timer that reschedules itself 0.1 ms ahead runs once per frame')

local handled = 0
server.world.addEventHandler({
  onEvent = function()
    handled = handled + 1
  end,
})
host:load_mission('second')
assert(host:timer_count() == 0, 'a timer survived a mission load')
assert(server.timer.getTime() == 0, 'model time did not reset')
host:event({ id = 1 })
assert(handled == 1, 'the event handler did not survive the load')
S.ok('a mission load clears timers, keeps handlers, and resets model time')

-- Event dispatch has no pcall.
local later = 0
local host2 = mock.new()
host2.states.server.world.addEventHandler({
  onEvent = function()
    error('first handler fails')
  end,
})
host2.states.server.world.addEventHandler({
  onEvent = function()
    later = later + 1
  end,
})
S.raises('first handler fails', host2.event, host2, { id = 1 })
assert(later == 0, 'a later handler saw the event')
S.ok('a raising event handler stops later handlers and reaches the caller')

-- A gui-state file handle has no seek, and its other methods work.
local scratch = os.tmpname()
local handle = assert(gui.io.open(scratch, 'wb'))
assert(handle.seek == nil, 'a gui file handle has seek')
assert(handle:write('abc') == nil, 'a gui write returned a value on success')
handle:close()
assert(gui.lfs.attributes(scratch, 'size') == 3)
os.remove(scratch)
S.ok('a gui-state file handle has no seek and write returns nil; lfs.attributes gives the size')

-- log.write is recorded.
gui.log.write('TEST', gui.log.INFO, 'hello')
local last = host.dcslog[#host.dcslog]
assert(last.subsystem == 'TEST' and last.level == 64 and last.message == 'hello')
S.ok('log.write lines are recorded with their level')
