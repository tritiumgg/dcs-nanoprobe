-- lua/experiments/loadout/player-name.lua under the strict mock: one unit with
-- a player name and one AI unit in the server state. The snippet finishes,
-- lists the player unit with a string name and its ammunition rows, and
-- counts the AI unit's name as nil. The mock has no `coalition.getPlayers`,
-- so the test gives it one that returns the player unit.

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

local server = host.states.server
for _, u in ipairs({ { 'np-player', 'np-person' }, { 'np-ai', nil } }) do
  server.coalition.addGroup(server.country.id.USA, server.Group.Category.GROUND, {
    name = u[1], task = 'Ground Nothing', visible = false, start_time = 0, x = 0, y = 0,
    units = { { name = u[1] .. '-1', type = 'M-113', x = 0, y = 0, heading = 0, skill = 'Average' } },
    route = { points = {} },
  })
  if u[2] then
    host.sim:set_player(host.sim.by_name[u[1] .. '-1'], u[2])
  end
end
rawset(server.coalition, 'getPlayers', function(side)
  local unit = server.Unit.getByName('np-player-1')
  if unit and unit:getCoalition() == side then
    return { unit }
  end
  return {}
end)

local source = S.read(root .. 'lua/experiments/loadout/player-name.lua')
assert(source, 'no player-name.lua')
assert((source:match('%-%- states: ([%a ]+)\n') or ''):find('server', 1, true), 'player-name.lua names the server state')
S.write(base .. 'in/loadout1.server.lua', source)
local result, text
for _ = 1, 30 * 60 do
  host:frame(1 / 60)
  text = S.read(base .. 'out/loadout1.json')
  if text then
    result = S.json(text)
    break
  end
end
assert(result, 'player-name.lua: no result in 30 s')
assert(result.ok and result.reply.ok, 'player-name.lua: ' .. text)
local seen = {}
for _, o in ipairs(result.reply.observations or {}) do
  seen[o.name] = o.value
end
assert(#seen.player_units == 1, 'one player unit: ' .. text)
local row = seen.player_units[1]
assert(row:find('type=M-113', 1, true) and row:find('name=string:np-person', 1, true), row)
assert(row:find('ammo=%[.+=%d+'), 'the player unit has ammunition rows: ' .. row)
assert(seen.ai_read >= 1 and seen.ai_names['nil:nil'] == seen.ai_read, 'every AI name is nil: ' .. text)
S.ok('player-name.lua: ' .. row .. '; AI ' .. seen.ai_read .. ' x nil')

-- ai-birth.lua: an M-113 is born 1 km from the player unit, named by no
-- player, and its group is gone when the snippet returns.
source = S.read(root .. 'lua/experiments/loadout/ai-birth.lua')
assert(source, 'no ai-birth.lua')
assert((source:match('%-%- states: ([%a ]+)\n') or ''):find('server', 1, true), 'ai-birth.lua names the server state')
S.write(base .. 'in/loadout2.server.lua', source)
result = nil
for _ = 1, 30 * 60 do
  host:frame(1 / 60)
  text = S.read(base .. 'out/loadout2.json')
  if text then
    result = S.json(text)
    break
  end
end
assert(result and result.ok and result.reply.ok, 'ai-birth.lua: ' .. tostring(text))
seen = {}
for _, o in ipairs(result.reply.observations or {}) do
  seen[o.name] = o.value
end
assert(tonumber(seen.ai_unit) and seen.ai_player_name == 'nil', 'ai-birth.lua: ' .. text)
host:frame(1 / 60)
local left = server.Group.getByName('np-loadout-ai')
assert(left == nil or not left:isExist(), 'ai-birth.lua left its group')
S.ok('ai-birth.lua: unit ' .. seen.ai_unit .. ', no player name, group removed')
