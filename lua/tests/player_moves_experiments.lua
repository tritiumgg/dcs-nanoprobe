-- The player-moves snippet of lua/experiments/player-moves under the strict
-- mock: one connected client, one slot offered to each coalition. The test
-- fills the placeholders as run.sh does, runs a list of steps, and checks
-- each step's record. A waiting step is released by the test changing the
-- client's side itself, as a person or a client snippet would.

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

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = false })

host.players[2] = { ucid = 'client-ucid', name = 'np-person', side = 0, slot = '', ping = 0 }
host.slots = { red = { { unitId = '41', type = 'Mi-8MT' } }, blue = { { unitId = '77', type = 'UH-1H' } } }
local hook = host.states.hook
rawset(hook.net, 'get_my_player_id', function()
  return 1
end)

local function eval(id, source, seconds, during)
  S.write(base .. 'in/' .. id .. '.hook.lua', source)
  for i = 1, seconds * 60 do
    host:frame(1 / 60)
    if during then
      during(i / 60)
    end
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      return S.json(text), text
    end
  end
  error(id .. ': no result in ' .. seconds .. ' s')
end

local template = S.read(root .. 'lua/experiments/player-moves/moves.lua')
assert((template:match('%-%- states: ([%a ]+)\n') or ''):find('hook', 1, true), 'moves.lua names the hook state')
local serial = 0
local function run(steps, seconds, during)
  local source = template:gsub('<steps>', steps):gsub('<settle>', '1')
  serial = serial + 1
  local result, text = eval('moves' .. serial, source, seconds, during)
  assert(result.ok, steps .. ': ' .. tostring(result.error))
  return result.reply, text
end

local function seen(reply)
  local out = {}
  for _, o in ipairs(reply.observations or {}) do
    out[o.name] = o.value
  end
  return out
end

do
  local reply, text = run('1,;2,;2,;2,@blue;2,;1,@blue;0,', 20)
  assert(reply.ok, 'sides: ' .. tostring(reply.error) .. ' ' .. text)
  local o = seen(reply)
  assert(o.offered.blue[1].unitId == '77' and o.offered.blue[1].id_type == 'string', 'sides: the blue slot is offered')
  assert(o.offered.red[1].unitId == '41', 'sides: the red slot is offered')
  -- Each step: the call's side and slot, before, after, and what it returned.
  -- Step 6 names the blue slot with red: the mock returns false and changes
  -- nothing, as DCS did (LIVE).
  local want = {
    { 1, '', 0, '', 1, '', 'true' }, { 2, '', 1, '', 2, '', 'true' }, { 2, '', 2, '', 2, '', 'true' },
    { 2, '77', 2, '', 2, '77', 'true' }, { 2, '', 2, '77', 2, '', 'true' }, { 1, '77', 2, '', 2, '', 'false' },
    { 0, '', 2, '', 0, '', 'true' },
  }
  for i, w in ipairs(want) do
    local s = o['step_' .. i]
    assert(s, 'sides: step ' .. i .. ' recorded')
    assert(s.ok == true and s.side == w[1] and s.slot == w[2], 'sides: step ' .. i .. ' called with its side and slot')
    assert(s.before.side == w[3] and s.before.slot == w[4], 'sides: step ' .. i .. ' before')
    assert(s.after.side == w[5] and s.after.slot == w[6], 'sides: step ' .. i .. ' after')
    assert(s.returned == w[7], 'sides: step ' .. i .. ' returned ' .. tostring(s.returned))
  end
  assert(o.step_3.changed_at == nil, 'sides: a move to the side the client is on changes nothing')
  assert(o.step_2.changed_at ~= nil and o.step_2.first_change.side == 2, 'sides: the change is timed')
  S.ok('sides: seven steps recorded with before, first change and after')
end

do
  host.players[2].side, host.players[2].slot = 2, ''
  local changed = false
  local reply, text = run('2,,1', 10, function(t)
    if t > 2 and not changed then
      changed = true
      host.players[2].side = 1
    end
  end)
  assert(reply.ok, 'race: ' .. tostring(reply.error) .. ' ' .. text)
  local s = seen(reply).step_1
  assert(s.waited and s.waited >= 1.5, 'race: the step waited for the change, ' .. tostring(s.waited))
  assert(s.before.side == 1 and s.after.side == 2, 'race: the call was made after the change and set side 2')
  S.ok('race: waits for the side change, then calls in the same frame')
end

do
  -- The client's half, run as if this state were the client's: its own ID is
  -- 2, and set_coalition changes its side a second later, as a server would
  -- confirm it.
  host.players[2].side = 2
  local pending
  rawset(hook.net, 'get_my_player_id', function()
    return 2
  end)
  rawset(hook.net, 'set_coalition', function(side, password)
    assert(type(side) == 'number' and password == '', 'set_coalition(side, password)')
    pending = side
  end)
  local source = S.read(root .. 'lua/experiments/player-moves/client-side.lua'):gsub('<side>', '1')
  local result, text = eval('client-side', source, 20, function(t)
    if pending and t > 1 then
      host.players[2].side, pending = pending, nil
    end
  end)
  assert(result.ok and result.reply.ok, 'client side: ' .. text)
  local o = seen(result.reply)
  assert(o.before.side == 2 and o.side_1.side == 2 and o.side_2.side == 1, 'client side: the change is recorded')
  rawset(hook.net, 'get_my_player_id', function()
    return 1
  end)
  S.ok('client side: set_coalition called, the side change recorded with its time')
end

do
  host.players[2] = nil
  local result = eval('moves-none', (template:gsub('<steps>', '0,'):gsub('<settle>', '1')), 5)
  assert(result.ok and not result.reply.ok and tostring(result.reply.error):find('no client connected', 1, true),
    'no client: refused')
  S.ok('no client: the snippet fails with its reason')
end
