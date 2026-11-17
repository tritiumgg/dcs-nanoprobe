-- The warehouse step snippet of lua/experiments/warehouse under the strict
-- mock. Two airbases: one limited (counts below 1,000,000) and one unlimited.
-- A stand-in player unit sits in the server state, and a connected client
-- with an offered slot in the hook state. The test fills the placeholders as
-- run.sh does and checks each action's observations; the mock does not take
-- stores from a warehouse, so the test changes a count itself during a watch
-- and checks the snippet records it.

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

local sim = host.sim
local limited = {
  id = 9001, name = 'np-limited', coalition = 2, x = 0, z = 0,
  stock = { aircraft = { ['UH-1H'] = 4 }, weapon = { ['weapons.bombs.Mk_82'] = 10 },
    liquids = { [0] = 5000, [1] = 0, [2] = 0, [3] = 0 } },
}
-- The unlimited stand-in reads 1,000,000, as E6 did (S15). The warehouse
-- run's mission read its unlimited warehouses below that (results doc).
local unlimited = {
  id = 9002, name = 'np-unlimited', coalition = 2, x = 10000, z = 0,
  stock = { aircraft = { ['UH-1H'] = 1000000 },
    weapon = { ['weapons.bombs.Mk_82'] = 1000000, ['weapons.missiles.AIM_9'] = 1000000 },
    liquids = { [0] = 1000000, [1] = 1000000, [2] = 1000000, [3] = 1000000 } },
}
sim.airbases[#sim.airbases + 1] = limited
sim.airbases[#sim.airbases + 1] = unlimited

local server = host.states.server
server.coalition.addGroup(server.country.id.USA, server.Group.Category.GROUND, {
  name = 'np-player', task = 'Ground Nothing', visible = false, start_time = 0,
  x = 0, y = 0,
  units = { { name = 'np-player-1', type = 'M-113', x = 0, y = 0, heading = 0, skill = 'Client' } },
  route = { points = {} },
})
rawset(server.coalition, 'getPlayers', function(side)
  local unit = server.Unit.getByName('np-player-1')
  if unit and unit:getCoalition() == side then
    return { unit }
  end
  return {}
end)
host.players[2] = { ucid = 'client-ucid', name = 'np-person', side = 0, slot = '', ping = 0 }
local hook = host.states.hook
rawset(hook.DCS, 'getAvailableSlots', function(side)
  if side == 'blue' then
    return { { unitId = '77', type = 'UH-1H', role = 'pilot' } }
  end
  return {}
end)
rawset(hook.net, 'get_my_player_id', function()
  return 1
end)
rawset(hook.net, 'get_slot', function(id)
  local p = host.players[id]
  return p and p.side, p and p.slot
end)

local function eval(id, source, seconds, during, state)
  S.write(base .. 'in/' .. id .. '.' .. (state or 'hook') .. '.lua', source)
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

local template = S.read(root .. 'lua/experiments/warehouse/step.lua')
assert((template:match('%-%- states: ([%a ]+)\n') or ''):find('hook', 1, true), 'step.lua names the hook state')
local serial = 0
local function run(action, slot, seconds, during)
  local source = template:gsub('<action>', action):gsub('<slot>', slot or ''):gsub('<seconds>', tostring(seconds or 3))
    :gsub('<stock>', '50')
  serial = serial + 1
  local result, text = eval('warehouse' .. serial, source, (seconds or 3) + 10, during)
  assert(result.ok, action .. ': ' .. tostring(result.error))
  assert(result.reply.ok, action .. ': ' .. tostring(result.reply.error) .. ' ' .. text)
  local seen = {}
  for _, o in ipairs(result.reply.observations or {}) do
    seen[o.name] = o.value
  end
  return seen, text
end

do
  local seen = run('list')
  assert(#seen.slots == 1 and seen.slots[1]:find('unitId=77', 1, true), 'list: the offered slot')
  assert(seen.airbases:find('np-limited\t0\t9001\ttrue\ttrue', 1, true), 'list: the limited airbase ' .. seen.airbases)
  assert(seen.airbases:find('np-unlimited\t0\t9002\ttrue\tfalse', 1, true), 'list: the unlimited airbase')
  S.ok('list: ' .. #seen.slots .. ' slot, ' .. #seen.players .. ' connections')
end

do
  local seen = run('stock')
  -- The mock's own airbases have small counts, so they are limited too.
  local n = tonumber(tostring(seen.stock):match('^(%d+) limited, 3 item names, (%d+) sets$'))
  assert(n and seen.stock:find(' ' .. (3 * n) .. ' sets', 1, true), 'stock: ' .. tostring(seen.stock))
  assert(limited.stock.aircraft['UH-1H'] == 50, 'stock: the limited warehouse holds 50 UH-1H')
  assert(limited.stock.weapon['weapons.missiles.AIM_9'] == 50 and limited.stock.weapon['weapons.bombs.Mk_82'] == 50,
    'stock: the limited warehouse holds 50 of each weapon')
  assert(unlimited.stock.weapon['weapons.bombs.Mk_82'] == 1000000, 'stock: the unlimited warehouse is unchanged')
  S.ok('stock: ' .. seen.stock)
end

do
  local changed = false
  local seen = run('slot', '77', 3, function(t)
    if t > 1.5 and not changed then
      changed = true
      limited.stock.weapon['weapons.bombs.Mk_82'] = 46
      limited.stock.liquids[0] = 4200
    end
  end)
  assert(host.players[2].side == 2 and host.players[2].slot == '77', 'slot: the client is in slot 77')
  assert(seen.action.ok == true and seen.action.side == 2, 'slot: force_player_slot ran')
  assert(seen.start.limited['np-limited'] == '0 id 9001' and not seen.start.limited['np-unlimited'], 'slot: only the limited warehouse is watched')
  assert(#seen.changes == 1, 'slot: one change seen, got ' .. #seen.changes)
  local net = table.concat(seen.net, '; ')
  assert(net:find('np-limited weapon weapons.bombs.Mk_82: 50 -> 46 (-4)', 1, true), 'slot: weapon delta ' .. net)
  assert(net:find('np-limited liquids 0: 5000 -> 4200 (-800)', 1, true), 'slot: fuel delta ' .. net)
  assert(#seen['end'].players == 1, 'slot: the player unit is read')
  S.ok('slot: ' .. net)
end

do
  local seen = run('deslot')
  assert(host.players[2].side == 0 and host.players[2].slot == '', 'deslot: the client is a spectator')
  assert(seen.action.ok == true and #seen.net == 0, 'deslot: ran, no change')
  S.ok('deslot: force_player_slot to spectators')
end

do
  local seen = run('kill')
  assert(tostring(seen.action.result):find('explosion at np-player-1', 1, true), 'kill: ' .. tostring(seen.action.result))
  S.ok('kill: ' .. seen.action.result)
end

do
  local seen = run('none')
  assert(seen.action.action == 'none' and #seen.changes == 0, 'none: watch only')
  S.ok('none: watch only')
end

-- unlimited-read.lua in the server state. Without a mission `warehouses`
-- table the snippet picks the airbase whose by-name read is 1,000,000, runs
-- E6's control there, leaves the count as it was, and settles nothing. With
-- the flags readable it holds for 1,000,000 reads and fails for a lower one.
do
  local source = S.read(root .. 'lua/experiments/warehouse/unlimited-read.lua')
  local function read(what)
    local result, text = eval('unlimited-' .. what, source, 10, nil, 'server')
    assert(result.ok and result.reply.ok, 'unlimited-read ' .. what .. ': ' .. text)
    local seen = {}
    for _, o in ipairs(result.reply.observations or {}) do
      seen[o.name] = o.value
    end
    return seen, text
  end
  local seen, text = read('unreadable')
  assert(seen.control and seen.control.airbase == 'np-unlimited', 'unlimited-read: the control ran on ' .. text)
  assert(seen.flags_readable == false and seen.checked == 0 and seen.verdict == 'inconclusive',
    'unlimited-read: unreadable flags settled something: ' .. text)
  assert(unlimited.stock.weapon['weapons.bombs.Mk_82'] == 1000000, 'unlimited-read: the count is as it was')
  local server_env = host.states.server.env
  rawset(server_env, 'mission', { warehouses = { airports = {
    [9001] = { unlimitedAircrafts = false, unlimitedMunitions = false, unlimitedFuel = false },
    [9002] = { unlimitedAircrafts = true, unlimitedMunitions = true, unlimitedFuel = true,
      aircrafts = { a = 1, b = 1 }, weapons = { a = 1, b = 1 } },
  } } })
  -- An unlimited warehouse reads 1,000,000 for any item (E6); the mock reads
  -- only what it holds.
  unlimited.stock.aircraft['F-15C'] = 1000000
  seen, text = read('holds')
  assert(seen.flags_readable == true and seen.checked == 3 and seen.verdict == 'holds',
    'unlimited-read: 1,000,000 reads did not hold: ' .. text)
  unlimited.stock.liquids[2] = 999
  seen, text = read('fails')
  unlimited.stock.liquids[2] = 1000000
  assert(seen.verdict == 'fails' and seen.failed[1] == 'np-unlimited: fuel',
    'unlimited-read: a lower read did not fail: ' .. text)
  rawset(server_env, 'mission', nil)
  unlimited.stock.aircraft['F-15C'] = nil
  S.ok('unlimited-read: inconclusive without flags, holds at 1,000,000, fails below')
end
