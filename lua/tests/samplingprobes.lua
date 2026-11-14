-- The read-only cost probes of lua/sampling under the strict mock, with the
-- host-native agent installed under the DLL's name: each runs in the state
-- its header names over a small mixed scenario and returns its figures. The
-- figures themselves mean nothing on the mock; this is the rehearsal before
-- a probe runs on a server.

local MODULE = assert(..., 'usage: lua lua/tests/samplingprobes.lua <module path>')
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
S.copy(root .. 'lua/dev/lib/agent.lua', profile .. 'nanoprobe/dev/lua/agent.lua')

local BUILD = '0123456789abcdef'
S.mkdir(profile .. 'nanoprobe/versions/' .. BUILD)
S.copy(MODULE, profile .. 'nanoprobe/versions/' .. BUILD .. '/nanoprobe_agent_' .. BUILD .. '.dll')
S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = false })

local function eval(id, state, source, seconds)
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, seconds * 60 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.ok, id .. ': ' .. tostring(result.error))
      assert(result.reply.ok, id .. ': ' .. tostring(result.reply.error))
      return result.reply.values[1]
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

local spawned = eval('spawn', 'server',
  "local E = ...\nreturn E.lib('scenario').spawn_mixed(60, { label = 'p', moving = 0.5 })\n", 30)
assert(type(spawned) == 'table' and spawned.ground > 0, 'the scenario did not spawn')
for _ = 1, 120 do
  host:frame(1 / 60)
end

local server = eval('server', 'server', S.read(root .. 'lua/sampling/perf-server.lua'), 120)
assert(server.classes.units.rows >= 50, 'the server probe found too few units')
assert(not server.classes.units.life.raised and server.classes.units.life.lua >= 0 and server.classes.units.life.csweep_bare >= 0, 'no life figures')
assert(server.classes.units.by_id.rows == server.classes.units.rows, 'the getter did not cover every unit')
assert(server.enumeration.units >= 50 and #server.enumeration.calls > 0, 'no enumeration figures')
assert(server.misc.agent_clock >= 0, 'no call figures')
S.ok(string.format('perf-server.lua: %d classes, %d units, %d groups', #server.order,
  server.classes.units.rows, server.enumeration.groups))

local hook = eval('hook', 'hook', S.read(root .. 'lua/sampling/perf-hook.lua'), 120)
assert(hook.has_getter == 'function', 'the hook state lacks the position getter')
assert(hook.classes.units.rows == server.classes.units.rows, 'the hook probe got other units than the server probe')
assert(hook.classes.units.csweep_rows >= 0 and hook.classes.units.lua >= 0, 'no row figures')
assert(hook.overheads.rows == hook.classes.units.rows and hook.overheads.c_function_csweep >= 0, 'no call figures')
assert(hook.frame.get_pause >= 0 and hook.xcall.return_only.median >= 0, 'no frame or call figures')
S.ok(string.format('perf-hook.lua: %d classes, %d units', #hook.order, hook.classes.units.rows))

-- perf-frames.lua takes the agent's table in the server state from the
-- shim, which this test does not load: a stand-in holds it.
eval('standin', 'server', "local E = ...\n_G.__nanoprobe_shim = { agent = E.lib('agent').open() }\nreturn true\n", 30)
local frames = eval('frames', 'hook', S.read(root .. 'lua/sampling/perf-frames.lua'), 120)
eval('standin-off', 'server', '_G.__nanoprobe_shim = nil\nreturn true\n', 30)
assert(frames.units == hook.classes.units.rows and frames.frames == 240, 'the frames probe did not run its frames')
assert(frames.hook_csweep_20.n == 240 and frames.hook_lua_20.median >= 0, 'no per-frame position figures')
assert(frames.server_agent and frames.server_vitals_20.n == 240 and frames.server_vitals_20_call.median >= 0,
  'no per-frame vitals figures: agent ' .. tostring(frames.server_agent) .. ', frames '
    .. tostring(frames.server_vitals_20.n) .. ', ' .. tostring(frames.error))
S.ok(string.format('perf-frames.lua: %d frames over %d units', frames.frames, frames.units))

for _, name in ipairs({ 'reads.lua', 'getters.lua' }) do
  local out = eval(name:gsub('%.lua$', ''), 'hook', S.read(root .. 'lua/sampling/' .. name), 120)
  assert(type(out) == 'table', name .. ' returned no table')
  S.ok(name .. ': ran')
end

-- The design probes add objects of their own and remove them.
for _, name in ipairs({ 'design-objects', 'design-combat' }) do
  local out = eval(name, 'server', S.read(root .. 'lua/sampling/' .. name .. '.lua'), 400)
  assert(type(out) == 'table', name .. '.lua returned no table')
  if name == 'design-objects' then
    assert(out.candidates['np-design-windsock'].found, 'the windsock was not added')
    assert(#out.scale == 3 and out.scale[3].added == 1000, 'the static list was not grown to 1,000')
  else
    assert(out.units['np-design-b1'] and out.units['np-design-n1'], 'the probe watched no unit')
    assert(next(out.fuel) ~= nil, 'no fuel was read')
  end
  S.ok(name .. '.lua: ran')
end
local left = eval('left', 'server', [[
local n = 0
for side = 0, 2 do
  for _, s in ipairs(coalition.getStaticObjects(side) or {}) do
    if s:getName():find('^np%-design') then n = n + 1 end
  end
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    if g:getName():find('^np%-design') then n = n + 1 end
  end
end
return n
]], 30)
assert(left == 0, 'the design probes left ' .. tostring(left) .. ' objects')

-- Nothing is left behind by the probes: the scenario's objects are the
-- test's own.
eval('clear', 'server', "local E = ...\nreturn E.lib('scenario').clear()\n", 30)
local result
S.write(base .. 'in/cleanup.uninstall.lua', '')
for _ = 1, 600 do
  host:frame(1 / 60)
  local text = S.read(base .. 'out/cleanup.json')
  if text then
    result = S.json(text)
    break
  end
end
assert(result and result.ok, 'the cleanup did not answer')
for _, report in ipairs(result.reply) do
  assert(#report.errors == 0, report.state .. ': ' .. tostring(report.errors[1]))
  assert(#report.new_globals == 0, report.state .. ' kept the global ' .. tostring(report.new_globals[1]))
end
S.ok('the probes left no global behind')

S.remove(profile)
