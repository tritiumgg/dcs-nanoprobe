-- A stand-in for DCS for the host chaos run (`cargo xtask chaos host`): the
-- hook and the shim in the strict mock, with the host-native agent, a mission
-- loaded paused, and frames at about 100 a second until the process is
-- killed. `chaos-runner` starts it and kills it.
--
-- usage: lua lua/chaos/host.lua <module> <profile> <map> <port> <token>
--
-- The first run installs into <profile>; a later one finds it installed and
-- only starts, as DCS does after a restart.

local root = (arg[0]:match('^(.*)lua/chaos/[^/]+$'))
assert(root, 'run this from the repository')
root = root == '' and './' or root
local S = dofile(root .. 'lua/tests/support.lua')
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, profile, map, port, token = ...
assert(MODULE and profile and map and port and token,
  'usage: lua lua/chaos/host.lua <module> <profile> <map> <port> <token>')
profile = profile:gsub('/*$', '/')

local BUILD = '0123456789abcdef'
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'

if not S.read(profile .. 'nanoprobe/manifest.txt') then
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
  S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 256\nin_ring_kib = 256\n", map, tonumber(port)))
  S.write(profile .. 'nanoprobe/token', token)
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
end

local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
host:load_mission('chaos', { paused = true })

-- Frames by CPU time, which keeps close to real time while the loop spins.
local now = os.clock
local next_frame = now()
while true do
  if now() >= next_frame then
    next_frame = next_frame + 0.01
    host:frame(0.01)
  end
end
