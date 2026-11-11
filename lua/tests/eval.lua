-- The eval channel (spec/eval.md): the hook and its prelude, installed as
-- `deploy --dev` lays them out and run in the strict mock.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local function install()
  local profile = S.scratch()
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(profile .. 'nanoprobe/dev/lua')
  for _, folder in ipairs({ 'in', 'taken', 'out' }) do
    S.mkdir(profile .. 'nanoprobe/dev/eval/' .. folder)
  end
  S.copy(root .. 'lua/dev/eval-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
  S.copy(root .. 'lua/dev/eval-prelude.lua', profile .. 'nanoprobe/dev/lua/prelude.lua')
  return profile
end

local profile = install()
local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
assert(host.dcslog[1].message == 'eval hook ready', tostring(host.dcslog[1].message))
host:load_mission('first')

local serial = 0

-- Drop a request and run frames until its result appears.
local function eval(state, source, frames)
  serial = serial + 1
  local id = string.format('t%03d', serial)
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, frames or 120 do
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      local result = S.json(text)
      assert(result.id == id and result.state == state, text)
      return result, id
    end
  end
  return nil, id
end

-- A value, in each kind of state.
local result, id = eval('hook', [[return 1, 'two', nil, { a = true }, type(DCS)]])
assert(result.ok and result.reply.ok, 'the hook snippet failed')
assert(result.reply.n == 5 and result.reply.values[1] == 1 and result.reply.values[2] == 'two')
assert(result.reply.values[3] == S.null and result.reply.values[4].a == true)
assert(result.reply.values[5] == 'table')
assert(result.hook.gen == 1 and result.hook.frames == 0 and result.hook.paused == false)
assert(not S.read(base .. 'in/' .. id .. '.hook.lua'), 'the request is still in in/')
assert(S.read(base .. 'taken/' .. id .. '.hook.lua'), 'the request is not in taken/')
S.ok('a hook snippet returns its values with their count, and its file moves to taken/')

result = eval('server', [[leak = 5; return type(timer), type(net), E.state, leak]])
assert(result.reply.values[1] == 'table' and result.reply.values[2] == 'nil')
assert(result.reply.values[3] == 'server' and result.reply.values[4] == 5)
assert(host.states.server.leak == nil, 'a global a snippet assigned leaked into the state')
S.ok('a server snippet runs there, and a global it assigns stays its own')

result = eval('sse', [[return type(io), type(net.dostring_in), E.state]])
assert(result.ok and result.reply.ok, result.error or result.reply.error)
assert(result.reply.values[1] == 'nil' and result.reply.values[2] == 'function')
assert(result.reply.values[3] == 'sse')
result = eval('mission', [[return type(a_do_script), type(os)]])
assert(result.reply.values[1] == 'function' and result.reply.values[2] == 'nil')
S.ok('the sandbox is reached through a_do_script, and the mission state by name')

result = eval('nosuch', 'return 1')
assert(result.ok == false and result.error == 'the reply is a nil, not a string', result.error)
S.ok('a state DCS does not know gives an error, not a reply')

-- Failures inside a snippet.
result = eval('server', [[error('boom')]])
assert(result.ok and result.reply.ok == false and result.reply.error:find('boom', 1, true))
result = eval('server', [[error({})]])
assert(result.reply.ok == false and result.reply.error:find('error value of type table', 1, true))
result = eval('server', [[return (]])
assert(result.reply.ok == false and result.reply.error:find('compile:', 1, true), result.reply.error)
S.ok('a raise, a raise with a table, and a compile error are replies with ok false')

-- The encoder.
result = eval('server', [[
  local cycle = {}
  cycle.self = cycle
  local deep = {}
  local at = deep
  for _ = 1, 12 do
    at.d = {}
    at = at.d
  end
  local long = {}
  for i = 1, 2500 do
    long[i] = i
  end
  return cycle, deep, long, print, 0 / 0, 1 / 0, 'caf\233 \0', { [1] = 'a', [3] = 'c' }, 0.1, 2 ^ 60,
    string.rep('x', 50000), {}, 'h\195\169'
]])
local v = result.reply.values
assert(v[1].self == '<cycle>')
local depth, at = 0, v[2]
while type(at) == 'table' do
  depth, at = depth + 1, at.d
end
assert(at == '<depth>' and depth == 8, 'depth ' .. depth)
assert(#v[3] == 2001 and v[3][2001] == '<more>')
assert(v[4] == '<function>' and v[5] == '<nan>' and v[6] == '<inf>')
assert(v[7] == 'caf\233 \0', 'bytes that are not UTF-8 did not survive')
assert(v[8]['1'] == 'a' and v[8]['3'] == 'c')
assert(v[9] == 0.1 and v[10] == 2 ^ 60 and #v[11] == 50000 and next(v[12]) == nil)
assert(v[13] == 'h\195\169')
S.ok('the encoder marks cycles, depth, length, and values JSON cannot hold; a long reply crosses whole')

-- Observations.
result = eval('server', [[
  E.observe('same', { 1, 'a' }, { windows = { 1, 'a' } })
  E.observe('other', 2, { windows = 3, wine = 2 })
  E.observe('near', 0.4896, { windows = 0.49, within = 0.01 })
  E.observe('time', 5, { windows = 20, timing = true })
  E.observe('slow', 500, { windows = 20, wine = 20, timing = true })
  E.observe('mock-only', 'x', { windows = 'y', mock = 'x' })
  E.observe('bare', 7)
  E.note('a note')
]])
local o = result.reply.observations
assert(#o == 7 and o[1].name == 'same' and o[1].kind == 'exact')
assert(o[1].verdict.windows == 'match' and o[1].verdict.mock == 'match' and o[1].verdict.wine == nil)
assert(o[2].verdict.windows == 'differ' and o[2].verdict.wine == 'match' and o[2].verdict.mock == 'match')
assert(o[3].verdict.windows == 'match' and o[3].within == 0.01)
assert(o[4].kind == 'timing' and o[4].verdict.windows == 'match')
assert(o[5].verdict.windows == 'differ' and o[5].verdict.wine == 'differ' and o[5].verdict.mock == 'match')
assert(o[6].verdict.windows == 'differ' and o[6].verdict.mock == 'match')
assert(o[7].verdict == nil and o[7].value == 7 and result.reply.notes[1] == 'a note')
S.ok('an observation is judged per platform: exactly, within a bound, or as a timing')

-- The timing helper.
result = eval('server', [[
  local b = E.bench(function() return timer.getTime() end, 1000, 3)
  return b.clock, b.n, b.ns >= 0, type(E.clock())
]])
v = result.reply.values
assert(v[1] == 'DCS.getRealTime' and v[2] == 1000 and v[3] == true and v[4] == 'number')
result = eval('mission', [[return E.bench(function() end, 10).clock]])
assert(result.reply.values[1] == 'none')
S.ok('bench times with the best clock the state has, and says so when it has none')

-- Waiting.
result = eval('server', [[
  local fired
  E.track.timer(function(_, t) fired = t end, nil, timer.getTime() + 0.5)
  return E.wait(function()
    if fired then
      return 'fired', fired > 0
    end
  end, 5)
]])
assert(result.reply.ok and result.reply.values[1] == 'fired' and result.reply.values[2] == true)
assert(result.hook.frames >= 29 and result.hook.frames <= 32, 'frames ' .. result.hook.frames)
result = eval('server', [[return E.wait(function() end, 0.5)]])
assert(result.reply.ok == false and result.reply.error:find('timed out after 0.5 s', 1, true))
result = eval('server', [[return E.wait(function() error('late') end, 5)]])
assert(result.reply.ok == false and result.reply.error:find('late', 1, true))
S.ok('a snippet that waits finishes on a later frame, times out, or fails')

-- Crossing from the gui state.
result = eval('hook', [[
  local ok, t = E.cross('server', 'return type(timer.getTime())')
  local bad, why = E.cross('server', 'error("no")')
  local ok2, n = E.cross('sse', 'return 41 + 1')
  return ok, t, bad, why:find('no', 1, true) ~= nil, ok2, n
]])
v = result.reply.values
assert(v[1] == true and v[2] == 'number' and v[3] == false and v[4] == true and v[5] == true and v[6] == '42')
result = eval('server', [[return pcall(E.cross, 'gui', 'return 1')]])
assert(result.reply.values[1] == false)
S.ok('a hook snippet runs code in another state and reads back one value')

-- The callback log.
result = eval('hook', [[
  local names = {}
  for _, entry in ipairs(E.hook.since(0)) do
    names[#names + 1] = entry.name
  end
  local counts = E.hook.counts()
  return table.concat(names, ','), counts.frames > 0, counts.frames_loading, E.hook.seq()
]])
v = result.reply.values
assert(v[1] == 'onNetMissionChanged,onMissionLoadBegin,onPlayerStart,onMissionLoadEnd,onSimulationStart', v[1])
assert(v[2] == true and v[3] == 0 and v[4] == 5)
S.ok('the hook logs the callbacks DCS makes, and counts frames')

-- A library.
S.write(profile .. 'nanoprobe/dev/lua/sample.lua', [[
  local L = ...
  loads = (loads or 0) + 1
  return { state = L.state, twice = function(n) return n * 2 end }
]])
result = eval('server', [[
  local a, b = E.lib('sample'), E.lib('sample')
  return a == b, a.state, a.twice(21), pcall(E.lib, '../x')
]])
v = result.reply.values
assert(v[1] == true and v[2] == 'server' and v[3] == 42 and v[4] == false)
result = eval('mission', [[return pcall(E.lib, 'sample')]])
assert(result.reply.values[1] == false and result.reply.values[2]:find('no io', 1, true))
S.ok('a library loads once from the development folder, where the state has io')

-- Cleanup. A timer from before a mission load is not removed, because the
-- load cleared it and its ID may be another timer's since.
eval('server', [[
  E.track.global('np_test_global', 1)
  E.track.timer(function(_, t) return t + 1 end, nil, timer.getTime() + 100)
  E.track.handler({ onEvent = function() error('a handler that raises') end })
  E.track.defer('marker', function() np_deferred_ran = true end)
  E.store.kept = 'yes'
  _G.np_untracked = true
]])
assert(host.states.server.np_test_global == 1 and #host.handlers == 1 and host:timer_count() == 1)
host:event({ id = 1 })
result = eval('server', [[return E.store.kept]])
assert(result.reply.values[1] == 'yes')
assert(result.reply.notes[1]:find('fault: handler:', 1, true), 'a raising handler left no fault')
host:load_mission('second')
local kept
host.states.server.timer.scheduleFunction(function()
  kept = true
end, nil, 2)
eval('server', [[E.track.timer(function() end, nil, timer.getTime() + 100)]])
assert(host:timer_count() == 2)

result = eval('reset', '')
assert(result.ok and #result.reply >= 3, 'reset reports')
local report
for _, r in ipairs(result.reply) do
  if r.state == 'server' then
    report = r
  end
end
-- Three timers: one that fired while a snippet waited, and one from each mission.
assert(report.cleaned.global == 1 and report.cleaned.timer == 3 and report.cleaned.handler == 1)
assert(report.cleaned.deferred == 1 and #report.errors == 0)
assert(#report.new_globals == 1 and report.new_globals[1] == 'np_untracked', table.concat(report.new_globals, ','))
assert(host.states.server.np_test_global == nil and #host.handlers == 0)
assert(host:timer_count() == 1, 'reset removed a timer that was not its own')
for _ = 1, 150 do
  host:frame(1 / 60)
end
assert(kept, 'the other timer did not run')
result = eval('server', [[return E.store.kept]])
assert(result.reply.values[1] == S.null)
S.ok('reset removes what snippets tracked, reports an untracked global, and spares a timer of another mission')

-- Uninstall.
host.states.server.np_untracked = nil
result = eval('uninstall', '')
assert(result.ok)
for _, r in ipairs(result.reply) do
  assert(#r.errors == 0 and #r.new_globals == 0, r.state .. ' left something')
end
for _, state in ipairs({ 'hook', 'gui', 'server', 'mission', 'sse', 'config' }) do
  assert(rawget(host.states[state], '__nanoprobe_eval') == nil, state .. ' still holds the prelude')
end
result = eval('server', [[return 1]])
assert(result.reply.values[1] == 1, 'no eval after uninstall')
S.ok('uninstall takes the prelude out of every state, and the next request puts it back')

-- The kill switch.
S.write(profile .. 'nanoprobe/disable', '')
for _ = 1, 90 do
  host:frame(1 / 60)
end
assert(eval('hook', 'return 1', 120) == nil, 'a request ran after the kill switch')
host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
assert(#host.callbacks == 0 and host.dcslog[1].message:find('disabled by', 1, true))
S.ok('the kill switch stops the hook, and at start loads nothing')

S.remove(profile)
