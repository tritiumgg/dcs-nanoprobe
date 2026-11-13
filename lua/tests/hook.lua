-- The loader, the hook body and the shim, installed as spec/install.md lays
-- out and run in the strict mock's hook and server states with the
-- host-native agent.
--
-- The agent's link is opened once per process and its checks run once a
-- second of real time, so the cases that use the link run in one profile, in
-- order, and wait on the agent's clock. The kill switch comes last: it
-- disables the agent for the rest of the process.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local BUILD = '0123456789abcdef'

-- The host-native agent, which luatest.sh passes in. It is installed under
-- the DLL's name, and the hook opens it as DCS would open the DLL.
local MODULE = assert(..., 'usage: lua lua/tests/hook.lua <module path>')

-- The same module's table in this Lua state, for its clock and status.
-- The agent's table in this Lua state, opened from the first profile's
-- copy, the file the hook opens: on Linux another path would load another
-- copy, with state of its own.
local A

-- Seconds, by CPU time. Every wait here spins, so it keeps close to real
-- time, and it still runs after the kill switch stops every agent call.
local now = os.clock

-- A profile with the version installed, as deploy leaves it, with an
-- agent.toml that puts the mapped file in the profile and a token.
local function install()
  local profile = S.scratch()
  local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
  S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
  local port = 20000 + math.floor(os.clock() * 1e6) % 20000
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%snanoprobe/test.map'\nport = %d\nout_ring_kib = 256\nin_ring_kib = 256\n",
    profile, port))
  S.write(profile .. 'nanoprobe/token', string.rep('0123456789abcdef', 4) .. '\n')
  return profile
end

local function start(profile)
  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  return host
end

-- The messages of a log file's records, in order, and the records.
local function records(path)
  local msgs, all = {}, {}
  for _, line in ipairs(S.lines(path)) do
    local r = S.json(line)
    msgs[#msgs + 1] = r.msg
    all[#all + 1] = r
  end
  return msgs, all
end

local function has(list, msg)
  for i, m in ipairs(list) do
    if m == msg then
      return i
    end
  end
  return nil
end

-- Run frames until `done` returns true, or `seconds` of real time pass.
local function frames_until(host, seconds, done)
  local deadline = now() + seconds
  while now() < deadline do
    host:frame(1 / 60)
    if done() then
      return true
    end
  end
  return false
end

-- A normal start.
local profile = install()
A = assert(package.loadlib(profile .. 'nanoprobe/versions/' .. BUILD .. '/nanoprobe_agent_' .. BUILD .. '.dll',
  'luaopen_nanoprobe'))()
local hooklog = profile .. 'nanoprobe/logs/hook.log'
local shimlog = profile .. 'nanoprobe/logs/shim.log'
local host = start(profile)
local marker = host.dcslog[1]
assert(marker and marker.subsystem == 'NANOPROBE', 'no session marker in dcs.log')
assert(marker.message:match('^session %x+ build ' .. BUILD .. '$'), marker.message)
assert(#host.callbacks == 1, 'the hook registered ' .. #host.callbacks .. ' callback tables')

local msgs, all = records(hooklog)
assert(msgs[1] == 'hook loaded' and msgs[2] == 'agent loaded' and msgs[3] == 'link open'
  and msgs[4] == 'capabilities', table.concat(msgs, ', '))
assert(all[2].data.abi == '3', 'the agent loaded reads abi ' .. tostring(all[2].data.abi))
assert(all[3].data.map == profile .. 'nanoprobe/test.map', 'the link opened ' .. tostring(all[3].data.map))
for name, present in pairs(all[4].data) do
  assert(present == true, name .. ' is absent from the mock hook state')
end
local status = A.status()
assert(status.started and status.lifecycle == 1 and status.gen == 0,
  'before a mission the link reads lifecycle ' .. tostring(status.lifecycle) .. ', gen ' .. tostring(status.gen))
S.ok('the hook opens the agent and its link, and probes its capabilities')

host:load_mission('test mission')
host:frame()
msgs, all = records(hooklog)
local load = has(msgs, 'mission load begin')
assert(load and all[load].gen == 1 and all[load].data.mission == 'test mission', 'no mission load begin at gen 1')
assert(has(msgs, 'simulation start'), 'no simulation start')
local started = has(msgs, 'shim started')
assert(started, 'the shim did not start: ' .. table.concat(msgs, ', '))
assert(all[started].data.result == 'start 1 reloads 0', 'shim started reads ' .. tostring(all[started].data.result))
status = A.status()
assert(status.lifecycle == 3 and status.gen == 1,
  'after the start the link reads lifecycle ' .. tostring(status.lifecycle) .. ', gen ' .. tostring(status.gen))
local shim_msgs, shim_all = records(shimlog)
assert(shim_msgs[1] == 'shim loaded' and shim_msgs[2] == 'capabilities', table.concat(shim_msgs, ', '))
for name, present in pairs(shim_all[2].data) do
  assert(present == true, name .. ' is absent from the mock server state')
end
S.ok('the first simulation start loads the shim into the server state and probes its capabilities')

-- A second load starts the same shim again; the server state keeps it.
host:load_mission('test mission')
host:frame()
msgs, all = records(hooklog)
assert(all[#all].msg == 'shim started' and all[#all].data.result == 'start 2 reloads 0',
  tostring(all[#all].data.result))
assert(A.status().gen == 2, 'the generation did not move with the load')
S.ok('a reload keeps the shim and starts it again; the generation moves')

-- A pause and a resume are lifecycle changes the frame sees.
host:set_pause(true)
host:frame()
assert(A.status().lifecycle == 4, 'paused reads lifecycle ' .. A.status().lifecycle)
host:set_pause(false)
host:frame()
assert(A.status().lifecycle == 3, 'resumed reads lifecycle ' .. A.status().lifecycle)
S.ok('the frame follows DCS.getPause')

-- The shim reload request is seen within a second and runs the shim again.
S.write(profile .. 'nanoprobe/reload', '')
assert(frames_until(host, 1.5, function()
  return has(records(hooklog), 'shim reloaded') ~= nil
end), 'the hook did not reload the shim within a second')
assert(S.read(profile .. 'nanoprobe/reload') == nil, 'the reload request was not removed')
msgs, all = records(hooklog)
assert(all[#all].data.result == 'start 3 reloads 1', 'shim reloaded reads ' .. tostring(all[#all].data.result))
shim_msgs = records(shimlog)
assert(has(shim_msgs, 'shim reloaded'), 'shim.log has no reload')
S.ok('a reload request is seen within a second and runs the shim again, carrying its state')

-- A newer version whose shim raises while it loads: the reload fails and the
-- loaded shim stays.
local BAD = 'fedcba9876543210'
S.mkdir(profile .. 'nanoprobe/versions/' .. BAD .. '/lua')
S.copy(root .. 'lua/agent/log.lua', profile .. 'nanoprobe/versions/' .. BAD .. '/lua/log.lua')
S.write(profile .. 'nanoprobe/versions/' .. BAD .. '/lua/shim.lua', "error('a broken shim', 0)\n")
S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BAD .. '\n')
S.write(profile .. 'nanoprobe/reload', '')
assert(frames_until(host, 1.5, function()
  return has(records(hooklog), 'shim not reloaded') ~= nil
end), 'a raising shim was not reported')
msgs, all = records(hooklog)
assert(tostring(all[#all].data.error):find('a broken shim', 1, true), tostring(all[#all].data.error))
S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
host:load_mission('test mission')
host:frame()
msgs, all = records(hooklog)
assert(all[#all].msg == 'shim started' and all[#all].data.result == 'start 4 reloads 1',
  'after a failed reload the shim reads ' .. tostring(all[#all].data.result))
S.ok('a reload whose shim raises is reported, and the loaded shim stays')

-- The kill switch, created while running, is seen within a second.
S.write(profile .. 'nanoprobe/disable', '')
local found_at = now()
assert(frames_until(host, 1.5, function()
  return has(records(hooklog), 'kill switch found; stopping') ~= nil
end), 'the kill switch was not seen within a second')
assert(now() - found_at < 1.2, 'the kill switch took ' .. (now() - found_at) .. ' s')
status = A.status()
assert(status.lifecycle == 6, 'the kill switch left lifecycle ' .. tostring(status.lifecycle))
assert(status.disabled == true and status.reason == 'the kill switch',
  'status reads disabled ' .. tostring(status.disabled) .. ', reason ' .. tostring(status.reason))
assert(A.clock() == nil, 'an agent call still runs after the kill switch')
local agent_log = S.read(profile .. 'nanoprobe/logs/agent.log') or ''
assert(agent_log:find('"msg":"kill switch found; stopping"', 1, true), 'agent.log has no kill switch')
local count = #S.lines(hooklog)
local frame = status.frame
host:load_mission('another')
for _ = 1, 120 do
  host:frame(1 / 60)
end
assert(#S.lines(hooklog) == count, 'the hook kept working after the kill switch')
assert(A.status().frame == frame, 'the agent kept pumping after the kill switch')
S.ok('a kill switch created while running stops the agent and the hook within a second')

-- The kill switch present at start loads nothing.
host = start(profile)
assert(#host.callbacks == 0, 'a disabled install registered callbacks')
assert(host.dcslog[1].message:find('disabled by', 1, true), host.dcslog[1].message)
S.ok('a kill switch present at start loads nothing and says so in dcs.log')
S.remove(profile)

-- A version without its DLL is one error record, and the hook carries on.
profile = install()
os.remove(profile .. 'nanoprobe/versions/' .. BUILD .. '/nanoprobe_agent_' .. BUILD .. '.dll')
host = start(profile)
assert(#host.callbacks == 1, 'the hook stopped without its DLL')
local lines = S.lines(profile .. 'nanoprobe/logs/hook.log')
assert(lines[2] and lines[2]:find('"level":"error"', 1, true) and lines[2]:find('"msg":"agent not loaded"', 1, true),
  tostring(lines[2]))
-- Without the agent the hook looks for the kill switch itself.
host:load_mission('m')
S.write(profile .. 'nanoprobe/disable', '')
for _ = 1, 61 do
  host:frame(1 / 60)
end
lines = S.lines(profile .. 'nanoprobe/logs/hook.log')
assert(lines[#lines]:find('"msg":"kill switch found; stopping"', 1, true), lines[#lines])
S.ok('a missing DLL is one error record, and the hook carries on and keeps the kill switch')
S.remove(profile)

-- A hook written for another ABI refuses the DLL, and carries on.
profile = install()
local hookfile = profile .. 'nanoprobe/versions/' .. BUILD .. '/lua/hook.lua'
local text = S.read(hookfile):gsub("local ABI = '3'", "local ABI = '1'")
S.write(hookfile, text)
host = start(profile)
assert(#host.callbacks == 1, 'the hook stopped on an ABI mismatch')
lines = S.lines(profile .. 'nanoprobe/logs/hook.log')
assert(lines[2]:find('"msg":"agent not loaded"', 1, true) and lines[2]:find('ABI 3; this hook needs 1', 1, true),
  tostring(lines[2]))
S.ok('a hook of another ABI refuses the DLL with one error record and carries on')
S.remove(profile)

-- A missing manifest is one error line in dcs.log, and does not raise.
profile = install()
os.remove(profile .. 'nanoprobe/manifest.txt')
host = start(profile)
assert(#host.callbacks == 0)
assert(host.dcslog[1].level == 8 and host.dcslog[1].message:find('no manifest', 1, true),
  tostring(host.dcslog[1].message))
S.ok('a missing manifest is one error line in dcs.log and raises nothing')
S.remove(profile)
