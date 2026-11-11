-- The probe suite under the strict mock: every probe in lua/probes runs in
-- each state its header names, no observation differs from what is expected
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

local base = profile .. 'nanoprobe/dev/eval/'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-eval.lua')
-- A server with no client starts its mission paused.
host:load_mission(profile .. 'nanoprobe/dev/missions/np-shell.miz', { paused = true })

-- The probes, in name order.
local files = {}
local pipe = io.popen('ls ' .. S.quote(root .. 'lua/probes'))
for name in pipe:lines() do
  if name:match('%.lua$') then
    files[#files + 1] = name
  end
end
pipe:close()
table.sort(files)
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
      return S.json(text)
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
  local source = S.read(root .. 'lua/probes/' .. file)
  local states, timeout = header(source)
  assert(#states > 0, file .. ' names no state')
  for _, state in ipairs(states) do
    serial = serial + 1
    local label = file:gsub('%.lua$', '') .. '@' .. state
    local result = eval('p' .. serial, state, source, timeout)
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

S.remove(profile)
