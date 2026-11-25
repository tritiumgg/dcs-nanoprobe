-- The FARP-fixes snippets of lua/experiments/farp-fixes under the strict
-- mock, in their order. The mock's FARP behaves as DCS did live
-- (lua/mock/world.lua); what DCS reads back is the live run's. The test
-- checks each snippet finishes, adds and reads the statics, and leaves
-- nothing behind once the mission resumes.

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
local function run(file)
  local source = S.read(root .. 'lua/experiments/farp-fixes/' .. file)
  assert(source, 'no ' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find('server', 1, true), file .. ' does not name server')
  serial = serial + 1
  local id = 'farp' .. serial
  S.write(base .. 'in/' .. id .. '.server.lua', source)
  for _ = 1, 30 * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, file .. ': ' .. tostring(result.error))
      assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error) .. ' ' .. text)
      return result.reply.values[1], text
    end
  end
  error(file .. ': no result in 30 s')
end

local server = host.states.server
local names = { 'np-ff-farp', 'np-ff-pad', 'np-ff-bare', 'np-ff-unl' }
-- The airbases before the snippets add six heliports (four in spawn.lua, two
-- in frequency.lua); removed ones stay listed until the next load (LIVE).
local airbases_before = #host.sim.airbases

do
  local value, text = run('spawn.lua')
  for _, name in ipairs(names) do
    local o = value.objects[name]
    assert(o and o.added, name .. ' not added: ' .. text)
    assert(o.static and type(o.static.id) == 'number', name .. ' not read as a static: ' .. text)
  end
  S.ok('spawn.lua: four statics added and read')
end

do
  local value, text = run('destroy.lua')
  for _, name in ipairs({ 'np-ff-farp', 'np-ff-pad' }) do
    assert(tostring(value.calls[name]):find('^returned'), name .. ' not destroyed: ' .. text)
  end
  S.ok('destroy.lua: the two statics destroyed')
end

do
  local value, text = run('after.lua')
  -- On a paused server DCS holds a FARP's destroy until the simulation
  -- runs (LIVE, docs/results/2026-11-25-farp-fixes.md): the FARP is still
  -- found, and goes after a resume.
  assert(value.objects['np-ff-farp'] and value.objects['np-ff-farp'].static_by_name == true, text)
  host:set_pause(false)
  host:frame(1 / 60)
  host:set_pause(true)
  -- A removed FARP is still found by name, with `isExist` false, until the
  -- next mission load (LIVE).
  for _, name in ipairs(names) do
    local left = server.StaticObject.getByName(name)
    assert(left and not left:isExist(), name .. ' left after the cleanup')
  end
  assert(rawget(server, 'np_farp_fixes') == nil, 'the global is left')
  S.ok('after.lua: nothing left')
end

-- The server-state reads and the radio probe. The hook-state snippets
-- (`running.lua`, `pause-read.lua`, `reload.lua`) resume, pause and load the
-- mission, which this harness does not drive; they are not rehearsed.
do
  local value, text = run('frequency.lua')
  for _, name in ipairs({ 'np-ff-text', 'np-ff-num' }) do
    assert(value[name] and value[name].added and value[name].destroy == 'true', name .. ': ' .. text)
  end
  host:set_pause(false)
  host:frame(1 / 60)
  host:set_pause(true)
  for _, name in ipairs({ 'np-ff-text', 'np-ff-num' }) do
    local left = server.StaticObject.getByName(name)
    assert(left and not left:isExist(), 'frequency.lua left ' .. name)
  end
  S.ok('frequency.lua: two helipads added, read and destroyed')
end

do
  local value, text = run('left.lua')
  for _, name in ipairs(names) do
    local r = value[name]
    assert(r and type(r.id) == 'number' and r.exists == false and r.listed_exists == false, name .. ': ' .. text)
  end
  local count = run('count.lua')
  assert(count == airbases_before + 6, 'count.lua: ' .. tostring(count) .. ', not ' .. airbases_before + 6)
  S.ok('left.lua and count.lua: read')
end
