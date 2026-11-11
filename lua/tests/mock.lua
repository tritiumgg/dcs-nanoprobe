-- The strict mock DCS host raises on unknown and unmocked names and
-- reproduces the quirks its header lists.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local host = mock.new({ writedir = '/nowhere/' })
local gui, server = host.states.hook, host.states.server

-- Strictness.
S.raises('hook DCS.noSuchCall is not in the listing', function()
  return gui.DCS.noSuchCall()
end)
S.raises('hook DCS.exportToMiz is in the DCS API but not mocked', function()
  return gui.DCS.exportToMiz()
end)
S.raises('server timer.bogus is not in the listing', function()
  return server.timer.bogus
end)
assert(gui.Unit == nil and gui.world == nil, 'the hook state has no scripting API')
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
assert(dostring_in('sse', 'return 1') == nil, 'the sandbox has no state name')
assert(dostring_in('hook', 'return 1') == nil, 'the hook state has no state name')
gui.only_in_hook = true
assert(dostring_in('gui', 'return type(only_in_hook)') == 'nil', 'the gui state saw a hook global')
assert(select(2, dostring_in('gui', 'only_in_gui = true')) == true and gui.only_in_gui == nil,
  'the hook state saw a gui global')
gui.only_in_hook = nil
local text, ran = dostring_in('server', 'return {}')
assert(text == '' and ran == true, 'a table came back as ' .. tostring(text))
text, ran = dostring_in('server', 'error({})')
assert(text == '' and ran == false)
S.ok('dostring_in: own globals per state, a string and a flag back, nil for unknown names, gui apart from the hook')

-- The other states.
assert(dostring_in('mission', 'return type(os) .. type(io) .. type(package)') == 'nilnilnil')
assert(dostring_in('mission', 'return type(a_do_script)') == 'nil', 'a_do_script with no mission')
assert(dostring_in('config', 'return type(net)') == 'table')
assert(dostring_in('export', 'return 1') == nil, 'the export state exists with no mission')
S.ok('the mission state lacks os, io and package; config has net; export needs a mission')

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
for _ = 1, 6 do
  host:frame(1 / 60)
end
assert(ticks == 101, 'the timer ran ' .. ticks .. ' times in 100 ms')
assert(math.abs(server.timer.getTime() - 0.1) < 1e-9)
S.ok('a timer that reschedules itself 0.1 ms ahead runs once per millisecond, in a burst each frame')

local handled = {}
server.world.addEventHandler({
  onEvent = function(_, event)
    handled[#handled + 1] = event.id
  end,
})
host:load_mission('second')
assert(host:timer_count() == 0, 'a timer survived a mission load')
assert(server.timer.getTime() == 0, 'model time did not reset')
host:event({ id = 1 })
assert(table.concat(handled, ',') == '12,11,1', 'the handler saw ' .. table.concat(handled, ','))
S.ok('a mission load clears timers, keeps handlers, which see the mission end and start, and resets model time')

-- The sandbox, reached through a_do_script in the mission state.
local shifted = dostring_in('mission', [[
  local a, b, c = a_do_script('x = 5; return 1, 2, 3')
  return tostring(a) .. tostring(b) .. tostring(c)
]])
assert(shifted == 'nil12', shifted)
assert(dostring_in('mission', [[return select('#', a_do_script('return 1'))]]) == '1')
assert(host.states.sse.x == 5 and host.states.mission.x == nil, 'the chunk ran outside the sandbox')
assert(dostring_in('mission', [[local _, t = a_do_script('return type(io) .. type(net.dostring_in), 0') return t]])
  == 'nilfunction')
local before = host.states.sse
host:load_mission('third')
assert(host.states.sse ~= before and host.states.sse.x == nil, 'the sandbox survived a load')
S.ok('a_do_script shifts its results by one, and a mission load makes the sandbox again')

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

-- A hook-state file handle has no seek, and its other methods work.
local scratch = os.tmpname()
local handle = assert(gui.io.open(scratch, 'wb'))
assert(handle.seek == nil, 'a hook file handle has seek')
assert(handle:write('abc') == nil, 'a hook write returned a value on success')
handle:close()
assert(gui.lfs.attributes(scratch, 'size') == 3)
os.remove(scratch)
S.ok('a hook-state file handle has no seek and write returns nil; lfs.attributes gives the size')

-- log.write is recorded.
gui.log.write('TEST', gui.log.INFO, 'hello')
local last = host.dcslog[#host.dcslog]
assert(last.subsystem == 'TEST' and last.level == 64 and last.message == 'hello')
S.ok('log.write lines are recorded with their level')
