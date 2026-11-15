-- The S16 live-check snippets under the strict mock: the four checks in
-- lua/experiments/s16 (frame before onSimulationStart, emit's objects, a handler
-- surviving a load, the chat callbacks), the snippets the exit test's tool
-- sends (exit-*.lua), and the scripted client's snippets in
-- lua/experiments/alone (menu pick, observe, chat, slot race). Each runs in
-- the state its header names, finishes, and records what its rule needs; after
-- uninstall nothing is left in any state.
--
-- The client's snippets run in the hook state of a host that stands in for a
-- client: the test gives it the calls a client has and the mock lacks
-- (`Export.LoSetCommand`, `net.get_slot`, `net.set_slot` and the rest) and a
-- gui state with `Input.getEnvTable`. Their numbers and returns are made up
-- here and are the stub's; what DCS gives is what the live run reads.
--
-- The mock's gaps show as observations, as in the S15 snippets' test: it has no
-- `emit` (that needs the agent), and a mission load that runs no frame before
-- the start, so `frame_before_start` reads false here.

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
for _, lib in ipairs({ 'scenario', 's15', 'routes' }) do
  S.copy(root .. 'lua/scenarios/' .. lib .. '.lua', profile .. 'nanoprobe/dev/lua/' .. lib .. '.lua')
end

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

-- A client's stand-in: the calls the snippets use that the mock lacks.
local client = { commands = {}, slot = { 0, '' }, unit = 'nil', chats = {}, spawned = 0 }
do
  local hook = host.states.hook
  rawset(hook, 'Export', {
    LoSetCommand = function(n)
      client.commands[#client.commands + 1] = n
    end,
  })
  local net = hook.net
  rawset(net, 'get_my_player_id', function()
    return 2
  end)
  rawset(net, 'get_slot', function()
    return client.slot[1], client.slot[2]
  end)
  rawset(net, 'set_coalition', function(side)
    client.side = side
    return true
  end)
  rawset(net, 'set_slot', function(side, id)
    client.slot = { side, tostring(id) }
    return true
  end)
  rawset(net, 'spawn_player', function()
    client.spawned = client.spawned + 1
    client.unit = '8580'
    return true
  end)
  rawset(hook.DCS, 'getPlayerUnit', function()
    return client.unit
  end)
  rawset(hook.DCS, 'getAvailableSlots', function(side)
    if side == 'blue' then
      return { { unitId = '8580', type = 'A-10C_2' } }
    end
    return {}
  end)
  -- The gui state: the input numbers a client's Input table holds, and the
  -- radio panel's missionCommands.
  local gui = host.states.gui
  local input = {
    getEnvTable = function()
      return { Actions = { iCommandToggleCommandMenu = 9001, ICommandMenuItem10 = 9010, ICommandMenuItem1 = 9011 } }
    end,
  }
  rawset(gui, 'Input', input)
  -- A client's hook state reaches ED's Input module with require (LIVE, S16).
  host.states.hook.package.loaded.Input = input
  rawset(gui, 'missionCommands', {
    doAction = function(index)
      client.action = index
    end,
  })
end

local function dump(v)
  if type(v) ~= 'table' then
    return tostring(v)
  end
  local parts = {}
  for k, x in pairs(v) do
    parts[#parts + 1] = tostring(k) .. '=' .. dump(x)
  end
  table.sort(parts)
  return '{' .. table.concat(parts, ',') .. '}'
end

local function eval(id, state, source, seconds, each)
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, seconds * 60 do
    host:frame(1 / 60)
    if each then
      each()
    end
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      return S.json(text), text
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

local function observations(result, file)
  assert(result.ok, file .. ': ' .. tostring(result.error))
  assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error))
  local seen = {}
  for _, o in ipairs(result.reply.observations) do
    seen[o.name] = o
  end
  return seen
end

local serial = 0
local function run(dir, file, state, seconds, each)
  local source = S.read(root .. 'lua/experiments/' .. dir .. file)
  assert(source, 'no ' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find(state, 1, true), file .. ' does not name the state ' .. state)
  serial = serial + 1
  local result, text = eval('s16x' .. serial, state, source, seconds, each)
  return result, text, source
end

local function need(seen, file, names)
  for _, name in ipairs(names) do
    assert(seen[name], file .. ': no observation named ' .. name)
  end
end

-- The snippets of lua/experiments/s16.

do
  local file = 'e-emit-objects.lua'
  local result = run('s16/', file, 'server', 60)
  local seen = observations(result, file)
  need(seen, file, { 'emit_is_a_function', 'rows', 'equal_count', 'rows_count', 'cleared' })
  assert(seen.rows_count.value == 6, 'six objects: ' .. tostring(seen.rows_count.value))
  local rows = seen.rows.value
  assert(type(rows) == 'table' and #rows == 6, 'six rows')
  for _, row in ipairs(rows) do
    assert(row.id_ ~= nil and row.getID ~= nil, file .. ': a row has no id_ or getID: ' .. dump(row))
  end
  assert(seen.cleared.value == true, 'the snippet left objects')
  S.ok(file .. ': id_ and getID for 5 units and 1 static, and the objects removed')
end

do
  local file = 'e-frame-before-start.lua'
  local result = run('s16/', file, 'hook', 60)
  local seen = observations(result, file)
  need(seen, file, { 'order', 'frames_between_begin_and_end', 'frames_between_end_and_start',
    'frames_after_start', 'frame_before_start', 'samples' })
  local order = seen.order.value
  assert(order:find('onMissionLoadBegin onMissionLoadEnd onSimulationStart', 1, true), 'the order is ' .. order)
  assert(seen.frames_after_start.value >= 5, 'five frames after the start')
  S.ok(file .. ': ' .. order .. '; frames before the start: ' .. tostring(seen.frame_before_start.value))
end

do
  local file = 'e-handler-survives-load.lua'
  local result = run('s16/', file, 'hook', 60)
  local seen = observations(result, file)
  need(seen, file, { 'install', 'events_counted' })
  assert(seen.events_counted.value >= 2, 'the handler counted ' .. tostring(seen.events_counted.value) .. ' events')
  -- Without a load: the second run finds the same state and the handler.
  result = run('s16/', file, 'hook', 60)
  seen = observations(result, file)
  need(seen, file, { 'state_reset', 'count_before', 'count_after', 'handler_survived', 'removed' })
  assert(seen.state_reset.value == false and seen.handler_survived.value == true, 'no load: the handler survives')
  assert(seen.removed.value == 'removed', 'the handler was not removed')
  -- With a load between: the mock keeps the server state and its event
  -- handlers across a load (LIVE for handlers, per the mock's notes).
  run('s16/', file, 'hook', 60)
  host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })
  result = run('s16/', file, 'hook', 60)
  seen = observations(result, file)
  need(seen, file, { 'state_reset', 'handler_survived', 'count_before', 'removed' })
  assert(seen.state_reset.value == false and seen.handler_survived.value == true,
    'the mock keeps the handler across a load')
  -- A new Lua state, as when the load gives the server state a fresh one.
  run('s16/', file, 'hook', 60)
  rawset(host.states.server, '__np_s16_survive', nil)
  result = run('s16/', file, 'hook', 60)
  seen = observations(result, file)
  need(seen, file, { 'state_reset', 'handler_survived', 'count_before' })
  assert(seen.state_reset.value == true and seen.handler_survived.value == false, 'a reset state is reported')
  S.ok(file .. ': counted, survives with and without a load, and a new state is reported')
end

do
  local file = 'e36-chat-hooks.lua'
  local sent = false
  local result = run('s16/', file, 'hook', 60, function()
    if not sent and #host.callbacks > 2 then
      sent = true
      host.players[2] = { ucid = 'x', name = 'np-client', side = 0, slot = '' }
      host:chat(2, 'np-s16-plain', true)
      local replaced = host:chat(2, 'np-s16-r one', true)
      host.last_replaced = replaced
    end
  end)
  local seen = observations(result, file)
  need(seen, file, { 'installed', 'log', 'replacer_calls', 'observer_calls', 'observer_called_after_a_replacement' })
  assert(seen.replacer_calls.value == 2 and seen.observer_calls.value == 2,
    'both callbacks saw both lines: ' .. tostring(seen.replacer_calls.value) .. ' ' .. tostring(seen.observer_calls.value))
  -- The mock calls every callback set, so the observer saw the replaced line.
  assert(seen.observer_called_after_a_replacement.value == true, 'the mock called the observer after the replacer')
  S.ok(file .. ': two callback sets, both lines seen by both, the order recorded')
  -- Disabled once the snippet ended: no more calls are recorded.
  local after = host:callback_values('onPlayerTrySendChat', 2, 'np-s16-r late', true)
  for _, r in ipairs(after) do
    assert(r[1] == nil, 'a callback still returns after the snippet ended')
  end
end

-- The snippets the exit test's tool sends.

do
  local file = 'exit-mission-path.lua'
  local result = run('s16/', file, 'hook', 30)
  assert(result.ok and result.reply.ok, file)
  local path = result.reply.values[1]
  assert(type(path) == 'string' and path:find('np-shell.miz', 1, true), 'the path is ' .. tostring(path))
  S.ok(file .. ': ' .. path:match('[^/]+$'))
end

do
  local file = 'exit-marks-read.lua'
  local result = run('s16/', file, 'server', 30)
  assert(result.ok and result.reply.ok, file)
  assert(result.reply.values[1]:match('^0|%|panels=0$') or result.reply.values[1] == '0||panels=0',
    'no mark yet: ' .. tostring(result.reply.values[1]))
  -- A mark with an ID from the shim's range, and one below it.
  local server = host.states.server
  server.trigger.action.markToAll(2 ^ 30 + 5, 'np-s16-mark', { x = 1, y = 0, z = 2 }, true)
  server.trigger.action.markToAll(77, 'a player mark', { x = 1, y = 0, z = 2 }, false)
  result = run('s16/', file, 'server', 30)
  local read = result.reply.values[1]
  assert(read == string.format('1|%d=np-s16-mark|panels=2', 2 ^ 30 + 5), 'the read is ' .. tostring(read))
  server.trigger.action.removeMark(2 ^ 30 + 5)
  server.trigger.action.removeMark(77)
  S.ok(file .. ': counts the shim\'s panels apart from a player\'s')
end

do
  local file = 'exit-menu-pick.lua'
  local server = host.states.server
  local called
  server.missionCommands.addCommand('np-s16 test', nil, function()
    called = true
  end)
  local source = S.read(root .. 'lua/experiments/s16/' .. file):gsub("'<index>'", '1')
  local result = eval('s16xpick', 'server', source, 30)
  assert(result.ok and result.reply.ok and result.reply.values[1] == 'ok', file)
  assert(called, 'doAction(1) did not run the command')
  local unfilled = S.read(root .. 'lua/experiments/s16/' .. file)
  result = eval('s16xpick2', 'server', unfilled, 30)
  assert(not result.reply.ok, 'the snippet ran with its placeholder unfilled')
  S.ok(file .. ': doAction(index) runs the command; unfilled it refuses')
end

do
  local file = 'e34-ban-restart.lua'
  local source = S.read(root .. 'lua/experiments/s16/' .. file)
  local saved = host.players
  host.players = { [1] = saved[1] }
  host:connect(2, { ucid = string.rep('ab', 16), name = 'np-client', side = 2, slot = '8580' })
  local bans = #host.bans
  local function step(name, id)
    local result = eval(id, 'hook', (source:gsub("'<step>'", "'" .. name .. "'")), 30)
    assert(result.ok and result.reply.ok, file .. ' ' .. name .. ': ' .. dump(result))
    return observations(result, file)
  end
  local seen = step('ban', 's16xban1')
  assert(seen.listed_after_ban.value == 1, 'the ban step: ' .. dump(seen))
  -- The restart: the ban list stays, the player is gone.
  host.players = { [1] = saved[1] }
  seen = step('check', 's16xban2')
  assert(seen.listed_after_restart.value == 1 and seen.removed.value == 1 and seen.listed_after_remove.value == 0,
    'the check step: ' .. dump(seen))
  assert(#host.bans == bans, 'a ban was left')
  local text = dump(seen)
  assert(not text:find(string.rep('ab', 16), 1, true), 'a ucid was returned')
  host.players = saved
  S.ok(file .. ': bans the lone client, finds the ban after a restart, removes it, returns no ucid')
end

-- The scripted client's snippets.

local function alone(file, state, seconds, each)
  local result = run('alone/', file, state, seconds, each)
  return observations(result, file), result
end

do
  local file = 's16-menu-pick.lua'
  local seen = alone(file, 'hook', 30)
  need(seen, file, { 'route1_numbers', 'has_LoSetCommand', 'route2_gui', 'route2_tree', 'route1_sent' })
  local n = seen.route1_numbers.value
  assert(n.toggle == 9001 and n.item10 == 9010 and n.item1 == 9011, 'the numbers were ' .. dump(n))
  assert(#client.commands == 3 and client.commands[1] == 9001 and client.commands[2] == 9010
    and client.commands[3] == 9011, 'the commands sent were ' .. table.concat(client.commands, ','))
  S.ok(file .. ': read the input numbers at run time and sent them in order')
  -- A client with no Actions: the numbers are not found, nothing is sent.
  local loaded = host.states.hook.package.loaded
  local real = loaded.Input
  loaded.Input = { getEnvTable = function() return {} end }
  client.commands = {}
  seen = alone(file, 'hook', 30)
  assert(seen.route1 and seen.route1.value == 'the numbers were not found', 'route 1 ran without numbers')
  assert(#client.commands == 0, 'a command was sent without a number')
  loaded.Input = real
  S.ok(file .. ': with no numbers it sends nothing')
end

do
  local file = 's16-observe.lua'
  local source = S.read(root .. 'lua/experiments/alone/' .. file)
  -- Armed with 0: returns at once, then the next run reports.
  local armed = source:gsub("'<wait>'", '0')
  local result = eval('s16xarm', 'hook', armed, 30)
  assert(result.ok and result.reply.ok and result.reply.values[1] == 'armed', 'not armed')
  host:callback('onNetDisconnect', 'kicked: np-s16-kick', 5)
  host:callback('onPlayerChangeSlot', 2)
  host:frame(1 / 60)
  result = eval('s16xrep', 'hook', armed, 30)
  local seen = observations(result, file)
  need(seen, file, { 'report' })
  local report = seen.report.value
  assert(#report.disconnects == 1 and report.disconnects[1].reason == 'kicked: np-s16-kick'
    and report.disconnects[1].code == '5', 'the disconnect was not recorded')
  assert(#report.slot_changes == 1, 'the slot change was not recorded')
  assert(#report.samples >= 1, 'no sample')
  -- Disarmed: a callback after the report records nothing and a new run arms again.
  host:callback('onNetDisconnect', 'late', 1)
  result = eval('s16xrearm', 'hook', armed, 30)
  assert(result.reply.values[1] == 'armed', 'the observer did not arm again')
  result = eval('s16xrep2', 'hook', armed, 30)
  report = observations(result, file).report.value
  assert(#report.disconnects == 0, 'a disarmed callback recorded')
  -- With a wait: it returns the report itself.
  local waited = source:gsub("'<wait>'", '2')
  S.ok(file .. ': arms, records a disconnect and a slot change, reports, disarms')
  local result2 = eval('s16xwait', 'hook', waited, 30)
  assert(observations(result2, file).report, 'the waiting run gave no report')
  -- Unfilled, the wait is 30 s.
  seen = alone(file, 'hook', 45)
  need(seen, file, { 'report' })
  S.ok(file .. ': with a wait it reports after the wait, 30 s when it is not filled in')
end

do
  local file = 's16-chat.lua'
  local seen = alone(file, 'hook', 30)
  need(seen, file, { 'sent', 'shown_to_the_client' })
  assert(#seen.sent.value == 2 and seen.sent.value[1]:find('np-s16-plain', 1, true)
    and seen.sent.value[2]:find('np-s16-r one', 1, true), 'the lines sent were ' .. dump(seen.sent.value))
  S.ok(file .. ': sends the two lines')
end

do
  local file = 's16-slot-race.lua'
  client.slot, client.unit, client.spawned = { 0, '' }, 'nil', 0
  local seen = alone(file, 'hook', 30)
  need(seen, file, { 'slot', 'set_coalition', 'set_slot', 'spawn_player', 'get_slot', 'player_unit' })
  assert(client.spawned == 1 and client.slot[2] == '8580', 'the race did not take the slot and spawn')
  S.ok(file .. ': takes the slot and asks to spawn in one step')
end

-- Nothing is left behind.
local result = eval('cleanup', 'uninstall', '', 10)
assert(result.ok, tostring(result.error))
for _, report in ipairs(result.reply) do
  assert(#report.errors == 0, report.state .. ': ' .. tostring(report.errors[1]))
  assert(#report.new_globals == 0, report.state .. ' kept the global ' .. tostring(report.new_globals[1]))
  assert(report.handlers == S.null or report.handlers == 0, report.state .. ' kept an event handler')
end
for _ = 1, 5 do
  host:frame(1 / 60)
end
assert(#host.handlers == 0 and host:timer_count() == 0, 'a handler or a timer is left')
assert(next(host.sim.groups) == nil and next(host.sim.statics) == nil, 'a group or a static is left')
S.ok('the S16 snippets run under the mock and leave nothing after uninstall')

S.remove(profile)
