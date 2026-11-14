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

-- Doomed units, weapon kills, destroyed units and statics.
do
  local h = mock.new()
  local srv = h.states.server
  h:load_mission('doom')
  local usa = srv.country.id.USA
  local events = {}
  srv.world.addEventHandler({
    onEvent = function(_, e)
      events[#events + 1] = e
    end,
  })
  local function names()
    local out = {}
    for _, e in ipairs(events) do
      for name, id in pairs(srv.world.event) do
        if id == e.id then
          out[#out + 1] = name:gsub('^S_EVENT_', '')
        end
      end
    end
    return table.concat(out, ',')
  end
  local function spawn(group, name, x)
    h.sim:add_group(usa, 2, { name = group, units = { { name = name, type = 'M-113', x = x, y = 0 } } })
    local unit = h.sim.by_name[name]
    h:frame(1 / 60)
    events = {}
    return unit
  end
  local function frames(seconds)
    for _ = 1, math.floor(seconds * 10 + 0.5) do
      h:frame(0.1)
    end
  end
  local position = srv.DCS.getUnitPositionByObjectId

  -- An explosion kill with the default delay.
  local unit = spawn('g1', 'u1', 1000)
  local self = { id_ = unit.id }
  assert(srv.Unit.isDead(self) == false, 'a living unit is dead')
  local group = srv.Group.getByName('g1')
  assert(#group:getUnits() == 1)
  srv.trigger.action.explosion({ x = 1000, y = 0, z = 0 }, 100)
  assert(not srv.Object.isExist(self), 'a doomed unit exists')
  assert(#group:getUnits() == 0 and not group:isExist(), 'a doomed unit is listed')
  assert(srv.Unit.isDead(self) == true)
  assert(type(srv.Unit.getLife(self)) == 'number')
  assert(srv.Object.getCategory(self) == 1)
  assert(type(srv.Object.getPoint(self)) == 'table' and type(srv.Unit.getDesc(self)) == 'table')
  assert(select(1, position(unit.id)) == 1000, 'the wreck has no position')
  assert(names() == '', 'an explosion sent ' .. names() .. ' before the frame')
  h:frame(1 / 60)
  assert(names() == 'UNIT_LOST,DEAD', names())
  S.ok('an explosion kill: the unit stops existing, reads still answer, and with no delay the deaths come on the next frame')

  -- A delayed death.
  h.sim.death_delay = 30
  unit = spawn('g2', 'u2', 2000)
  self = { id_ = unit.id }
  srv.trigger.action.explosion({ x = 2000, y = 0, z = 0 }, 100)
  frames(29)
  assert(names() == '', 'a death came early: ' .. names())
  assert(srv.Unit.isDead(self) and select(1, position(unit.id)) == 2000)
  frames(2)
  assert(names() == 'UNIT_LOST,DEAD', names())
  S.ok('with death_delay 30 no death event comes before 30 s of model time')

  -- A weapon kill.
  local shooter = spawn('g3s', 'u3s', 5000)
  unit = spawn('g3', 'u3', 3000)
  self = { id_ = unit.id }
  h.sim:shoot(unit, shooter)
  assert(not srv.Object.isExist(self) and srv.Unit.isDead(self))
  h:frame(1 / 60)
  assert(names() == 'HIT,BDA', names())
  for _, e in ipairs(events) do
    assert(e.target.id_ == unit.id and e.target_data.object_id == unit.id and e.target_data.object_category == 1)
    assert(e.initiator.id_ == shooter.id)
  end
  frames(29)
  assert(names() == 'HIT,BDA', names())
  frames(2)
  assert(names() == 'HIT,BDA,KILL,UNIT_LOST,DEAD', names())
  assert(events[3].initiator.id_ == shooter.id and events[3].target.id_ == unit.id)
  S.ok('a weapon kill sends HIT and BDA naming the target at once, and KILL, UNIT_LOST and DEAD after the delay')

  -- A paused simulation holds a delayed death.
  unit = spawn('g4', 'u4', 4000)
  h.sim:shoot(unit)
  frames(1)
  events = {}
  h:set_pause(true, { silent = true })
  for _ = 1, 400 do
    h:frame(0.1)
  end
  assert(names() == '', 'a death came while paused: ' .. names())
  h:set_pause(false, { silent = true })
  frames(30)
  assert(names() == 'KILL,UNIT_LOST,DEAD', names())
  S.ok('a paused simulation delivers no delayed death')

  -- Destroyed units.
  h.sim.death_delay = 0
  local solo = spawn('g5', 'u5', 6000)
  local ids = { { id_ = solo.id } }
  local pair = spawn('g6', 'u6', 7000)
  ids[2] = { id_ = pair.id }
  srv.Unit.getByName('u5'):destroy()
  srv.Group.getByName('g6'):destroy()
  frames(0.5)
  assert(names() == '', 'a destroy sent ' .. names())
  for _, id in ipairs(ids) do
    S.raises('destroyed', srv.Unit.isDead, id)
    S.raises('destroyed', srv.Unit.getLife, id)
    S.raises('destroyed', srv.Object.getPoint, id)
    S.raises('destroyed', srv.Unit.getDesc, id)
    assert(srv.Object.getCategory(id) == 0)
    local x, y, z = position(id.id_)
    assert(x == 0 and y == 0 and z == 0)
  end
  S.raises('never existed', srv.Unit.isDead, { id_ = 12345 })
  S.ok('a destroyed unit sends no event, its reads raise, its category is 0 and its position is zeros')

  -- Statics.
  events = {}
  h.sim:add_static(usa, { name = 'st1', type = 'Fuel tank', x = 10, y = 10 })
  h.sim:add_static(usa, { name = 'st2', type = 'Container', x = 20, y = 20, category = 'Cargos' })
  h:frame(1 / 60)
  assert(names() == 'BIRTH,BIRTH', names())
  assert(events[1].initiator_data.object_category == 3 and events[1].initiator_data.unit_name == 'st1')
  assert(events[2].initiator_data.object_category == 6 and events[2].initiator.id_ == h.sim.statics.st2.id)
  S.ok('a static and a cargo static each send S_EVENT_BIRTH, with object categories 3 and 6')

  -- A static killed by a weapon, and one destroyed.
  h.sim.death_delay = 30
  events = {}
  local depot = h.sim:add_static(usa, { name = 'st3', type = 'Fuel tank', x = 30, y = 30 })
  local bunker = h.sim:add_static(usa, { name = 'st4', type = 'Fuel tank', x = 40, y = 40 })
  h:frame(1 / 60)
  events = {}
  local sid, bid = { id_ = depot.id }, { id_ = bunker.id }
  assert(srv.StaticObject.getLife(sid) > 0 and srv.Object.isExist(sid))
  h.sim:shoot(depot, shooter)
  assert(not srv.Object.isExist(sid) and srv.StaticObject.getLife(sid) <= 0, 'a killed static still exists')
  assert(srv.Object.getCategory(sid) == 3 and srv.StaticObject.getByName('st3') == nil)
  h:frame(1 / 60)
  assert(names() == 'HIT,BDA', names())
  assert(events[1].target.id_ == depot.id and events[1].target_data.object_category == 3)
  frames(31)
  assert(names() == 'HIT,BDA,KILL,UNIT_LOST,DEAD', names())
  srv.StaticObject.getByName('st4'):destroy()
  frames(0.5)
  assert(not srv.Object.isExist(bid) and names() == 'HIT,BDA,KILL,UNIT_LOST,DEAD', 'a removal sent an event')
  S.raises('no longer exists', srv.StaticObject.getLife, bid)
  -- A burning wreck: the destroy takes the killed static's reads away.
  srv.Object.destroy(sid)
  frames(0.5)
  S.raises('no longer exists', srv.StaticObject.getLife, sid)
  S.ok('a killed static reads life 0 and sends the unit events, a destroyed one raises and sends none')

  -- An explosion kills a static, and a HIT that kills nothing changes nothing.
  events = {}
  local tank = h.sim:add_static(usa, { name = 'st5', type = 'Fuel tank', x = 3000, y = 3000 })
  local alive = spawn('g9', 'u9', 9500)
  h:frame(1 / 60)
  events = {}
  srv.trigger.action.explosion({ x = 3000, y = 0, z = 3000 }, 100)
  assert(not srv.Object.isExist({ id_ = tank.id }) and srv.StaticObject.getLife({ id_ = tank.id }) <= 0)
  h.sim:hit(alive, tank)
  frames(0.5)
  assert(names() == 'HIT', names())
  assert(srv.Object.isExist({ id_ = alive.id }) and srv.Unit.isDead({ id_ = alive.id }) == false)
  frames(31)
  assert(names() == 'HIT,UNIT_LOST,DEAD', names())
  S.ok('an explosion kills a static with no HIT, and a HIT on a survivor leaves it alive')

  -- A destroy on a burning wreck, and a revival.
  events = {}
  local wreck = spawn('g7', 'u7', 8000)
  local wid = { id_ = wreck.id }
  h.sim:shoot(wreck, shooter)
  frames(1)
  assert(srv.Unit.isDead(wid))
  assert(srv.Unit.getByName('u7') == nil, 'a wreck has a name')
  srv.Object.destroy(wid)
  frames(1)
  S.raises('destroyed', srv.Unit.isDead, wid)
  frames(30)
  assert(names() == 'HIT,BDA,KILL,UNIT_LOST,DEAD', 'the wreck\'s death events did not come: ' .. names())
  events = {}
  local back = spawn('g8', 'u8', 9000)
  local bid8 = { id_ = back.id }
  h.sim:shoot(back, shooter)
  frames(1)
  assert(srv.Unit.isDead(bid8) and not srv.Object.isExist(bid8))
  h.states.mission.a_unit_set_life_percentage(back.mission_id, 50)
  assert(srv.Object.isExist(bid8) and srv.Unit.isDead(bid8) == false, 'the unit did not revive')
  assert(srv.Unit.getLife(bid8) > 0 and srv.Group.getByName('g8') ~= nil and #srv.Group.getByName('g8'):getUnits() == 1)
  assert(srv.Unit.getByName('u8').id_ == back.id, 'a revived unit has a new object ID')
  frames(31)
  assert(srv.Object.isExist(bid8), 'a late death event removed the revived unit')
  h.sim:shoot(back, shooter)
  assert(not srv.Object.isExist(bid8) and srv.Unit.isDead(bid8))
  S.ok('a destroy on a wreck takes its reads away, and a script that sets life revives a wreck with the same object ID')
end
