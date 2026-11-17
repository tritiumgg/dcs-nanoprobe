-- The person-test snippets of lua/experiments/person under the strict mock.
-- Each runs in the state its header names and finishes. A stand-in player
-- unit sits in the server state: the mock has no `coalition.getPlayers`, so
-- the test gives it one that returns that unit, and it adds a connected
-- player for `net.get_player_info`. What a person sees or hears is the live
-- run's; here the test checks the calls DCS gets (the mock's `sim.sounds`,
-- `sim.pictures` and `sim.picture_stops`), the callbacks' arguments and the
-- cleanup.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local profile = S.scratch()
S.mkdir(profile .. 'Scripts/Hooks')
S.mkdir(profile .. 'nanoprobe/dev/lua')
for _, folder in ipairs({ 'in', 'taken', 'out' }) do
  S.mkdir(profile .. 'nanoprobe/dev/eval/' .. folder)
end
S.copy(root .. 'lua/dev/eval-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
S.copy(root .. 'lua/dev/eval-prelude.lua', profile .. 'nanoprobe/dev/lua/prelude.lua')
S.copy(root .. 'lua/scenarios/scenario.lua', profile .. 'nanoprobe/dev/lua/scenario.lua')

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

local function eval(id, state, source, seconds)
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, seconds * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      return S.json(text), text
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

local serial = 0
local function run(file, state, seconds)
  local source = S.read(root .. 'lua/experiments/person/' .. file)
  assert(source, 'no ' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find(state, 1, true), file .. ' does not name the state ' .. state)
  serial = serial + 1
  local result, text = eval('person' .. serial, state, source, seconds or 30)
  assert(result.ok, file .. ': ' .. tostring(result.error))
  assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error) .. ' ' .. text)
  local seen = {}
  for _, o in ipairs(result.reply.observations or {}) do
    seen[o.name] = o
  end
  return result.reply, seen, text
end

-- The stand-in player: a ground unit, since the mock spawns those, returned
-- by coalition.getPlayers for its own side only.
local server = host.states.server
local o = { x = 0, z = 0 }
server.coalition.addGroup(server.country.id.USA, server.Group.Category.GROUND, {
  name = 'np-player', task = 'Ground Nothing', visible = false, start_time = 0,
  x = o.x, y = o.z,
  units = { { name = 'np-player-1', type = 'M-113', x = o.x, y = o.z, heading = 0, skill = 'Client' } },
  route = { points = {} },
})
rawset(server.coalition, 'getPlayers', function(side)
  local unit = server.Unit.getByName('np-player-1')
  if unit and unit:getCoalition() == side then
    return { unit }
  end
  return {}
end)
host.players[2] = { ucid = 'client-ucid', name = 'np-person', side = 2, slot = '1', ping = 0 }

do
  local _, seen = run('find-player.lua', 'server')
  assert(type(seen.players_in_units.value) == 'table' and #seen.players_in_units.value == 1, 'one player in a unit')
  assert(seen.players_in_units.value[1]:find('np-player-1', 1, true), 'the stand-in is found')
  S.ok('find-player.lua: ' .. seen.players_in_units.value[1])
end

-- What the mock recorded of the calls DCS got: `sim.sounds` and
-- `sim.pictures` (lua/mock/world.lua).
local sim = host.sim
local gid = server.Unit.getByName('np-player-1'):getGroup():getID()

local SOUND = {
  all = { call = 'outSound', target = nil },
  coalition = { call = 'outSoundForCoalition', target = 2 },
  group = { call = 'outSoundForGroup', target = gid },
}
for _, form in ipairs({ 'all', 'coalition', 'group' }) do
  local file = 'sound-' .. form .. '.lua'
  sim.sounds = {}
  local reply = run(file, 'server')
  assert(tostring(reply.values[1]):find(form .. ' played 3 times', 1, true), file .. ': ' .. tostring(reply.values[1]))
  assert(#sim.sounds == 3, file .. ': ' .. #sim.sounds .. ' sounds, not 3')
  for _, s in ipairs(sim.sounds) do
    assert(s.call == SOUND[form].call and s.target == SOUND[form].target and s.file == 'np-tone.wav',
      file .. ': ' .. tostring(s.call) .. ' ' .. tostring(s.target) .. ' ' .. tostring(s.file))
  end
  S.ok(file .. ': 3 x ' .. SOUND[form].call .. ' of np-tone.wav')
end

local PICTURE = {
  all = { call = 'a_out_picture', seconds = 10, clear_view = false },
  ['coalition-name'] = { call = 'a_out_picture_s', target = 'blue', seconds = 10, clear_view = false },
  group = { call = 'a_out_picture_g', target = gid, seconds = 10, clear_view = false },
  hold = { call = 'a_out_picture', seconds = 0, clear_view = false },
  clearview = { call = 'a_out_picture', seconds = 10, clear_view = true },
}
for _, name in ipairs({ 'all', 'coalition-name', 'group', 'hold', 'clearview', 'stop' }) do
  local file = 'picture-' .. name .. '.lua'
  sim.pictures, sim.picture_stops = {}, 0
  local reply, seen = run(file, 'hook')
  assert(tostring(reply.values[1]):find('^true nil'), file .. ' raised in the mission state: ' .. tostring(reply.values[1]))
  local want = PICTURE[name]
  if want then
    assert(#sim.pictures == 1, file .. ': ' .. #sim.pictures .. ' pictures, not 1')
    local p = sim.pictures[1]
    -- The mock's getValueResourceByKey returns a key it does not know as it is.
    local resolved = p.file == 'ResKey_Action_902' or p.file == 'l10n/DEFAULT/np-picture.png'
    assert(p.call == want.call and p.target == want.target and p.seconds == want.seconds
      and p.clear_view == want.clear_view and resolved,
      file .. ': DCS got ' .. tostring(p.call) .. ' ' .. tostring(p.target) .. ' ' .. tostring(p.file))
  else
    assert(#sim.pictures == 0 and sim.picture_stops == 1, file .. ': not one stop')
  end
  S.ok(file .. ': ' .. tostring(seen.call and seen.call.value))
end

-- The mock's a_out_picture_s takes the coalition's name, as the agent's
-- PICTURE sends it, and refuses a number: LIVE, a client shows nothing for one
-- (docs/results/2026-11-16-person-tests.md).
do
  local file = 'picture-coalition.lua'
  local reply = run(file, 'hook')
  assert(tostring(reply.values[1]):find('^false .*takes the arguments'), file .. ': ' .. tostring(reply.values[1]))
  S.ok(file .. ': the mock refuses the coalition as a number')
end

do
  local reply = run('kill-spawn.lua', 'server')
  assert(tostring(reply.values[1]):find('^spawned id'), 'kill-spawn.lua: ' .. tostring(reply.values[1]))
  local _, seen = run('kill-damage.lua', 'hook', 30)
  for _, name in ipairs({ 'before', 'set_life_0', 'after_1s', 'after_3s', 'after_10s' }) do
    assert(seen[name], 'kill-damage.lua: no observation named ' .. name)
  end
  assert(seen.set_life_0.value == 'ok', 'set life: ' .. tostring(seen.set_life_0.value))
  local cleared = run('kill-clear.lua', 'server')
  S.ok('kill: ' .. tostring(seen.after_10s.value) .. '; clear: ' .. tostring(cleared.values[1]))
end

do
  local added = run('menu-add.lua', 'server')
  assert(tostring(added.values[1]):find('added', 1, true), 'menu-add.lua: ' .. tostring(added.values[1]))
  -- A pick as ED's radio panel makes it (missionCommands.doAction, from 0).
  for index = 0, 20 do
    server.missionCommands.doAction(index)
  end
  local reply, seen = run('menu-read.lua', 'server')
  local calls = seen.callbacks.value
  assert(type(calls) == 'table' and #calls == 2, 'both callbacks ran once: ' .. tostring(reply.values[1]))
  for _, line in ipairs(calls) do
    assert(line:find('args n=1 string:arg%-'), 'a callback gets its own argument only: ' .. line)
  end
  assert(rawget(server, '__np_person_menu') == nil and rawget(server, '__np_person_paths') == nil, 'globals left')
  S.ok('menu: ' .. table.concat(calls, '; '))
end

do
  local _, seen = run('player-name.lua', 'hook')
  local rows = seen.players.value
  assert(type(rows) == 'table' and #rows == 2, 'two connections')
  assert(rows[2]:find('name=np-person', 1, true), 'the client name: ' .. tostring(rows[2]))
  for _, row in ipairs(rows) do
    assert(not row:find('ucid', 1, true), 'a row holds a ucid')
  end
  S.ok('player-name.lua: ' .. table.concat(rows, '; '))
end
