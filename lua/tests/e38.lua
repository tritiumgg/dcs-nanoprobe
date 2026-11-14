-- The E38 probes under the strict mock: each snippet in lua/experiments/e38
-- runs in the state its header names and finishes, and after uninstall
-- nothing is left in any state.
--
-- The mock models few unit types and none of ED's debriefing store, unit
-- database or the hook state's unit reads. So the test swaps the probes' unit
-- types for ones the mock has and shortens their runs, and for the probes
-- whose first DCS call the mock does not implement it expects the mock's own
-- refusal. A step that a probe reports as failed fails the test, unless it
-- is listed below with the call the mock lacks. Each probe must also record
-- the observations its report is built from.
-- What the probes found is in docs/results; this only proves they run.

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
      return S.json(text)
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

-- Unit types the probes use that the mock does not model, and a stand-in.
local STAND_INS = {
  ["'Ural%-375'"] = "'M-113'",
  ["'M%-109'"] = "'M-1 Abrams'",
  ["'M 818'"] = "'M-113'",
  ["'BTR%-80'"] = "'T-72B'",
  ["'FARP Fuel Depot'"] = "'Windsock'",
  ["'ammo_cargo', 'Cargos'"] = "'Windsock'",
  ["'M978 HEMTT Tanker'"] = "'M-113'",
}

-- file, state, seconds to wait, and the mock's refusal when one is expected.
local PROBES = {
  { 'hook-log.lua', 'hook', 10 },
  { 'hook-log.lua', 'gui', 10, 'DCS.setUserCallbacks is in the DCS API but not mocked' },
  { 'recon.lua', 'server', 60 },
  { 'coverage.lua', 'server', 120 },
  { 'others.lua', 'server', 120 },
  { 'classify.lua', 'server', 200 },
  { 'rearm.lua', 'server', 200 },
  { 'rules.lua', 'hook', 120 },
  { 'revive.lua', 'hook', 200 },
  { 'cost.lua', 'server', 280 },
  { 'store.lua', 'server', 10, 'DebriefingEventsData.getEvents is in the DCS API but not mocked' },
  { 'hook-reads.lua', 'hook', 60, 'DCS.getUnitProperty is in the DCS API but not mocked' },
  { 'trucks.lua', 'gui', 10, 'db.Units.Cars.Car is not in the listing' },
  -- The second run of the logger returns its log.
  { 'hook-log.lua', 'hook', 10 },
}

-- Steps the probes report as failed under the mock, each with the call the
-- mock does not implement. Any other failed step fails the test, and so
-- does a listed step that succeeds.
local EXPECTED_FAILURES = {
  ['others.lua'] = { fighter_kill = true }, -- the mock's first explosion kills the fighter
  ['classify.lua'] = {
    removals = { 'da1', 'da2', 'da3' }, -- trigger.action.deactivateGroup
    destroy_the_wrecks = { 'xd1', 'xd2', 'xd3' }, -- Unit.destroy on a plain table
  },
}

-- Observations each probe must record.
local REQUIRED = {
  ['recon.lua'] = { 'unit_class', 'reads_of_life', 'getUnitProperty_by_mission_id', 'getAmmo_tank' },
  ['coverage.lua'] = { 'changes_by_stage', 'changes', 'gone', 'event_counts', 'existed_when_the_event_came' },
  ['others.lua'] = { 'changes', 'objects', 'event_counts' },
  ['classify.lua'] = { 'cases', 'ground / Unit.destroy', 'static / explosion', 'aircraft / Unit.destroy' },
  ['rearm.lua'] = { '1 none', '2 M 818 at 25 m' },
  ['rules.lua'] = { 'ids', 'rules_before', 'rules_after_destroy', 'ns_a_call_warm' },
  ['revive.lua'] = { 'before', 'after_3_s', 'after_set_plus_120_s' },
  ['cost.lua'] = { 'cold_a_call', 'back_to_back_ns_a_call' },
}

-- Whether an observation's value says a step failed.
local function failed(value)
  if type(value) == 'string' then
    return value == 'failed or nothing returned' or value:sub(1, 12) == 'CROSS FAILED'
  end
  return false
end
-- classify.lua's steps record `done`, or the list of actions that failed.
local STEP_LISTS = { orders = true, explosions = true, removals = true, explosions_in_flight = true, destroy_the_wrecks = true }

for serial, probe in ipairs(PROBES) do
  local file, state, seconds, refusal = probe[1], probe[2], probe[3], probe[4]
  local source = S.read(root .. 'lua/experiments/e38/' .. file)
  assert((source:match('%-%- states: ([%a ]+)\n') or ''):find(state, 1, true), file .. ' does not name the state ' .. state)
  for from, to in pairs(STAND_INS) do
    source = source:gsub(from, to)
  end
  source = source:gsub('local RUN_S = %d+', 'local RUN_S = 100'):gsub('local RUN_S, STEP_S, WINDOW_S = %d+', 'local RUN_S, STEP_S, WINDOW_S = 45')
  local result = eval('e38x' .. serial, state, source, seconds)
  assert(result.ok, file .. ': ' .. tostring(result.error))
  if refusal then
    assert(not result.reply.ok and tostring(result.reply.error):find(refusal, 1, true),
      file .. ': expected the mock to refuse, got ' .. tostring(result.reply.error))
    S.ok(file .. '@' .. state .. ': stops at the call the mock does not implement')
  else
    assert(result.reply.ok, file .. ': ' .. tostring(result.reply.error))
    local expected, seen, failures = EXPECTED_FAILURES[file] or {}, {}, {}
    for _, o in ipairs(result.reply.observations) do
      seen[o.name] = true
      local step_failed = failed(o.value) or (file == 'classify.lua' and STEP_LISTS[o.name] and o.value ~= 'done')
      if step_failed then
        failures[o.name] = true
      end
      assert(not step_failed or expected[o.name], file .. ': the step ' .. o.name .. ' failed: ' .. tostring(o.value))
      -- An expected failure names the text its value must contain, or for a
      -- list of failed actions, exactly those labels.
      local want = expected[o.name]
      if step_failed and type(want) == 'string' then
        assert(tostring(o.value):find(want, 1, true),
          file .. ': the step ' .. o.name .. ' failed for another reason: ' .. tostring(o.value))
      elseif step_failed and type(want) == 'table' then
        local got = {}
        for _, label in ipairs(o.value) do
          got[#got + 1] = label
        end
        table.sort(got)
        assert(table.concat(got, ',') == table.concat(want, ','),
          file .. ': the step ' .. o.name .. ' failed for other actions: ' .. table.concat(got, ','))
      end
    end
    for name in pairs(expected) do
      assert(failures[name], file .. ': the step ' .. name .. ' is listed as failing under the mock and did not')
    end
    if file == 'hook-log.lua' and serial == #PROBES then
      local v = result.reply.values and result.reply.values[1]
      assert(type(v) == 'table' and type(v.counts) == 'table' and type(v.routes) == 'table' and type(v.entries) == 'table',
        file .. ': the second run did not return the log: ' .. tostring(v))
      assert(next(v.routes), file .. ': the logger recorded no route')
    end
    for _, name in ipairs(REQUIRED[file] or {}) do
      assert(seen[name], file .. ': no observation named ' .. name)
    end
    S.ok(string.format('%s@%s: %d observation(s)', file, state, #result.reply.observations))
  end
end
assert(#host.timer_errors == 0, 'a timer raised: ' .. tostring(host.timer_errors[1]))

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
S.ok('the E38 probes run under the mock and leave nothing after uninstall')

S.remove(profile)
