-- The probe suite under the strict mock: every probe in lua/probes, and
-- every catalogue scenario in lua/catalogue, runs in each state its header
-- names, no observation differs from what is expected
-- of the mock, and after uninstall nothing is left in any state.

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
S.copy(root .. 'lua/dev/lib/recorder.lua', profile .. 'nanoprobe/dev/lua/recorder.lua')
S.copy(root .. 'lua/dev/lib/pause.lua', profile .. 'nanoprobe/dev/lua/pause.lua')

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
-- A server with no client starts its mission paused.
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

-- The probes, in name order, then the catalogue scenarios.
local files = {}
for _, dir in ipairs({ 'lua/probes/', 'lua/catalogue/' }) do
  local names = {}
  local pipe = io.popen('ls ' .. S.quote(root .. dir))
  for name in pipe:lines() do
    if name:match('%.lua$') then
      names[#names + 1] = name
    end
  end
  pipe:close()
  table.sort(names)
  for _, name in ipairs(names) do
    files[#files + 1] = dir .. name
  end
end
assert(#files > 0, 'no probes')

-- A probe's settings: the leading comment lines `-- states:` and
-- `-- timeout:`, as spec/eval.md gives them.
local function header(source)
  local states, timeout = {}, 30
  for line in (source .. '\n'):gmatch('(.-)\n') do
    if line:sub(1, 2) ~= '--' then
      break
    end
    local list = line:match('^%-%-%s*states:%s*(.*)$')
    if list then
      for state in list:gmatch('%a+') do
        states[#states + 1] = state
      end
    end
    timeout = tonumber(line:match('^%-%-%s*timeout:%s*(%d+)')) or timeout
  end
  return states, timeout
end

-- A server pauses when its last client leaves, and anything that opens the
-- game port and closes it counts as a client (LOG). A monitor that checks the
-- port does that as often as it checks, so the test pauses the mock every 20
-- seconds, and the probes must resume it.
local frames, pauses = 0, 0
-- The catalogue scenarios' results, as `cargo xtask probes` reports them.
local catalogue_results = {}

local function eval(id, state, source, seconds)
  S.write(base .. 'in/' .. id .. '.' .. state .. '.lua', source)
  for _ = 1, seconds * 60 do
    frames = frames + 1
    if frames % 1200 == 0 and not host.paused then
      pauses = pauses + 1
      host:set_pause(true)
    end
    host:frame(1 / 60)
    local text = S.read(base .. 'out/' .. id .. '.json')
    if text then
      return S.json(text), text
    end
  end
  error(id .. ' in ' .. state .. ': no result in ' .. seconds .. ' s')
end

local observations, serial, failures = 0, 0, {}
local function check(condition, message)
  if not condition then
    failures[#failures + 1] = message
  end
  return condition
end
for _, file in ipairs(files) do
  local source = S.read(root .. file)
  local states, timeout = header(source)
  assert(#states > 0, file .. ' names no state')
  for _, state in ipairs(states) do
    serial = serial + 1
    local label = file:match('([^/]+)%.lua$') .. '@' .. state
    local result, raw = eval('p' .. serial, state, source, timeout)
    if file:match('^lua/catalogue/') then
      catalogue_results[#catalogue_results + 1] = string.format(
        '{"probe":"%s","state":"%s","ok":true,"result":%s}', label:match('^[^@]+'), state, raw)
    end
    if check(result.ok, label .. ': ' .. tostring(result.error))
      and check(result.reply.ok, label .. ': ' .. tostring(result.reply.error)) then
      for _, note in ipairs(result.reply.notes) do
        check(not note:find('^fault:'), label .. ': ' .. note)
      end
      local matched = 0
      for _, o in ipairs(result.reply.observations) do
        observations = observations + 1
        local verdict = o.verdict and o.verdict.mock
        check(verdict ~= 'differ', label .. ': ' .. o.name .. ' differs under the mock')
        matched = matched + (verdict == 'match' and 1 or 0)
      end
      S.ok(string.format('%s: %d observation(s), %d matched', label, #result.reply.observations, matched))
    end
  end
end
assert(#failures == 0, #failures .. ' failure(s):\n  ' .. table.concat(failures, '\n  '))
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
for name, state in pairs(host.states) do
  assert(rawget(state, '__nanoprobe_eval') == nil, name .. ' still holds the prelude')
end
assert(pauses > 0, 'the test never paused the mock')
S.ok(string.format('%d probes, %d observations, %d pause(s) survived, and nothing left after uninstall',
  #files, observations, pauses))

-- The catalogue builder reads what the scenarios reported under the mock:
-- the mock's births and deaths from 10-ground, with the initiator's id_ and
-- category, and the marks and the observed times from 50-marks.
local xtask = select(5, ...)
if xtask then
  local report = profile .. 'catalogue-probes.json'
  S.write(report, '{"kind":"mock","dcs":"mock","wine":null,"time":"now","results":['
    .. table.concat(catalogue_results, ',') .. ']}')
  local stem = profile .. 'catalogue'
  local ok = os.execute(S.quote(xtask) .. ' catalogue --build ' .. S.quote(report) .. ' --out ' .. S.quote(stem)
    .. ' > ' .. S.quote(profile .. 'build.txt') .. ' 2>&1')
  local md = S.read(stem .. '.md') or ''
  assert(ok == 0, 'catalogue --build failed:\n' .. (S.read(profile .. 'build.txt') or '') .. md)
  for _, needle in ipairs({
    '## S_EVENT_BIRTH (15)',
    '## S_EVENT_DEAD (8)',
    '| `initiator.id_` | number ',
    '## S_EVENT_MARK_ADDED (25)',
    '## S_EVENT_MARK_REMOVED (27)',
    '| `idx` | number ',
    'Values of `initiator_data.object_category`: 1: ',
    '50-marks: mark_added_time = ',
  }) do
    assert(md:find(needle, 1, true), 'the mock catalogue lacks ' .. needle .. ':\n' .. md)
  end
  S.ok('catalogue --build reads the scenarios\' own reports from the mock')
end

S.remove(profile)
