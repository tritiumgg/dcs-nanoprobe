-- The live snippets of the `ruins` scenario of `cargo xtask persistence`
-- (lua/live/ruins-read.lua, lua/live/ruins-kill.lua) under the strict mock:
-- a rehearsal before they run on a server. Each runs in the server state
-- through the eval channel, with its parameter line set as the scenario
-- sets it. The mock's dead static reads as the LIVE one does: found by
-- name, life 0, not existing (docs/results/2026-11-17-person-fixes.md).

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
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

local serial = 0
-- Run a snippet with its parameter line replaced, as the scenario does.
local function run(file, line, value)
  local source = S.read(root .. 'lua/live/' .. file)
  assert(source, 'no ' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find('server', 1, true), file .. ' does not name the server state')
  local at, to = source:find(line, 1, true)
  assert(at, file .. ' no longer holds the line `' .. line .. '`')
  source = source:sub(1, at - 1) .. value .. source:sub(to + 1)
  serial = serial + 1
  local id = 'ruins' .. serial
  S.write(base .. 'in/' .. id .. '.server.lua', source)
  for _ = 1, 30 * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, file .. ': ' .. tostring(result.error))
      assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error) .. ' ' .. text)
      -- The snippet's first value, as `cargo xtask` reads it.
      return result.reply.values[1]
    end
  end
  error(file .. ': no result in 30 s')
end

local function read(names)
  local quoted = {}
  for i, n in ipairs(names) do
    quoted[i] = '[[' .. n .. ']]'
  end
  return run('ruins-read.lua', 'local NAMES = {}', 'local NAMES = { ' .. table.concat(quoted, ', ') .. ' }')
end

local function kill(name)
  return run('ruins-kill.lua', "local NAME = ''", 'local NAME = [[' .. name .. ']]')
end

local server = host.states.server
local USA = server.country.id.USA
local LIVE, RUIN, NONE, NEAR = 'np#hGx5', 'np#hGx6', 'np#hGx7', 'np#hGx8'
server.coalition.addStaticObject(USA, { name = LIVE, type = 'FARP Fuel Depot', x = 100, y = 200, heading = 0 })
server.coalition.addStaticObject(USA, { name = RUIN, type = 'FARP Fuel Depot', x = 5000, y = 200, heading = 0, dead = true })
-- A live static as far from the kill as the scenario places its second one.
server.coalition.addStaticObject(USA, { name = NEAR, type = 'FARP Fuel Depot', x = 100, y = 1700, heading = 0 })

local seen = read({ LIVE, RUIN, NONE })
assert(seen[LIVE].found and seen[LIVE].life == 1 and seen[LIVE].exists == true, 'the live static reads wrong')
assert(seen[RUIN].found and seen[RUIN].life == 0 and seen[RUIN].exists == false, 'the static added dead reads wrong')
assert(seen[NONE].found == false and seen[NONE].life == nil, 'a static that is not there was found')
S.ok('ruins-read.lua: a live static, one added dead and one that is not there read apart')

assert(kill(NONE) == 'none', 'a kill of nothing did not say none')
assert(kill(LIVE) == 'ok', 'the kill did not run')
host:set_pause(false)
for _ = 1, 120 do
  host:frame(1 / 60)
end
host:set_pause(true)
seen = read({ LIVE, RUIN, NEAR })
assert(seen[LIVE].found and seen[LIVE].life == 0 and seen[LIVE].exists == false, 'the killed static reads alive')
assert(seen[RUIN].found and seen[RUIN].life == 0, 'the static added dead is no longer found')
assert(seen[NEAR].found and seen[NEAR].life == 1 and seen[NEAR].exists == true, 'the kill reached a static 1,500 m away')
S.ok('ruins-kill.lua: an explosion at the static kills it, and it is still found by name with life 0')
