-- The development mission loader takes a request once, renames it before
-- loading, refuses a bad name, and stops at the kill switch.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local profile = S.scratch()
S.mkdir(profile .. 'nanoprobe/dev/missions')
S.write(profile .. 'nanoprobe/dev/missions/shell.miz', 'zip')

local host = mock.new({ writedir = profile })
host:run('gui', root .. 'lua/dev/mission-loader.lua')
assert(host.dcslog[1].message == 'mission loader ready', tostring(host.dcslog[1].message))
host:load_mission('first')

local function seconds(n)
  for _ = 1, n * 60 do
    host:frame(1 / 60)
  end
end

S.write(profile .. 'nanoprobe/dev/load.txt', 'shell.miz\n')
seconds(2)
assert(#host.loads == 1, 'loads: ' .. #host.loads)
assert(host.loads[1] == profile .. 'nanoprobe/dev/missions/shell.miz', host.loads[1])
assert(host.mission == host.loads[1], 'the mission did not change')
assert(not S.read(profile .. 'nanoprobe/dev/load.txt'), 'the request is still there')
assert(S.read(profile .. 'nanoprobe/dev/load.taken'), 'the request was not renamed')
seconds(3)
assert(#host.loads == 1, 'a taken request loaded again')
S.ok('a request loads once and is renamed before the load')

S.write(profile .. 'nanoprobe/dev/load.txt', '../../Config/serverSettings.lua')
seconds(2)
assert(#host.loads == 1, 'a bad name was loaded')
assert(host.dcslog[#host.dcslog].message:find('refused a request', 1, true),
  host.dcslog[#host.dcslog].message)
S.ok('a name that is not a plain .miz file name is refused')

S.write(profile .. 'nanoprobe/disable', '')
S.write(profile .. 'nanoprobe/dev/load.txt', 'shell.miz')
seconds(2)
assert(#host.loads == 1, 'loaded after the kill switch')
S.ok('the kill switch stops the loader')

S.remove(profile)
