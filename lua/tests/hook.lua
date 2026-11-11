-- The loader and the hook body, installed as spec/install.md lays out and
-- run in the strict mock's gui state.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local BUILD = '0123456789abcdef'

-- A profile with the version installed, as deploy leaves it.
local function install()
  local profile = S.scratch()
  local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
  return profile
end

local function start(profile)
  local host = mock.new({ writedir = profile })
  host:run('gui', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  return host
end

local function records(profile)
  return S.lines(profile .. 'nanoprobe/logs/hook.log')
end

-- A normal start.
local profile = install()
local host = start(profile)
local marker = host.dcslog[1]
assert(marker and marker.subsystem == 'NANOPROBE', 'no session marker in dcs.log')
assert(marker.message:match('^session %x+ build ' .. BUILD .. '$'), marker.message)
assert(#host.callbacks == 1, 'the hook registered ' .. #host.callbacks .. ' callback tables')

host:load_mission('test mission')
host:frame()
local lines = records(profile)
assert(#lines == 3, 'expected 3 records, found ' .. #lines .. ':\n' .. table.concat(lines, '\n'))
assert(lines[1]:find('"target":"hook","msg":"hook loaded"', 1, true), lines[1])
assert(lines[1]:find('"gen":0,"build":"' .. BUILD .. '"', 1, true), lines[1])
assert(lines[2]:find('"msg":"mission load begin"', 1, true), lines[2])
assert(lines[2]:find('"gen":1', 1, true), lines[2])
assert(lines[2]:find('"mission":"test mission"', 1, true), lines[2])
assert(lines[3]:find('"msg":"simulation start"', 1, true), lines[3])
S.ok('the hook marks the session in dcs.log and logs the lifecycle as JSON lines')

-- The kill switch, created while running, is seen within a second.
S.write(profile .. 'nanoprobe/disable', '')
for _ = 1, 61 do
  host:frame(1 / 60)
end
lines = records(profile)
assert(lines[#lines]:find('"msg":"kill switch found; stopping"', 1, true), lines[#lines])
local count = #lines
host:load_mission('another')
for _ = 1, 120 do
  host:frame(1 / 60)
end
assert(#records(profile) == count, 'the hook kept working after the kill switch')
S.ok('a kill switch created while running stops the hook within a second')

-- The kill switch present at start loads nothing.
host = start(profile)
assert(#host.callbacks == 0, 'a disabled install registered callbacks')
assert(host.dcslog[1].message:find('disabled by', 1, true), host.dcslog[1].message)
S.ok('a kill switch present at start loads nothing and says so in dcs.log')
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
