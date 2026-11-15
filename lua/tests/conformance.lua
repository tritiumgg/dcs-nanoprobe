-- The conformance suite through RealSim on the host (step S17): the hook and
-- the shim in the strict mock, the host-native agent, and `conformance-probe`
-- as the sidecar, over a real mapped file and a real loopback handshake. The
-- suite's mission load is a MISSION_LOAD of a second mission; the mock loads
-- any name it is given.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/conformance.lua <module> <version> <link-probe> ...')
local CONFORMANCE_PROBE = PROBE:gsub('link%-probe$', 'conformance-probe')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
-- The whole run's limit, in CPU seconds of this loop; the probe has its own.
local LIMIT_S = 240

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'

S.mkdir(profile .. 'Scripts/Hooks')
S.mkdir(version .. 'lua')
S.mkdir(profile .. 'nanoprobe/logs')
S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
local port = 20000 + math.floor(os.clock() * 1e6 + os.time()) % 20000
S.write(profile .. 'nanoprobe/agent.toml', string.format(
  "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 1024\nin_ring_kib = 256\n", map, port))
S.write(profile .. 'nanoprobe/token', TOKEN)

local out = profile .. 'conformance.jsonl'
local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
host:load_mission('first', { paused = false })
assert(os.execute(string.format(
  '%s --map %s --port %d --token-file %s --mission second --seconds %d > %s 2>&1 & echo $! > %s',
  S.quote(CONFORMANCE_PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'),
  LIMIT_S - 20, S.quote(out), S.quote(profile .. 'probe.pid'))) == 0, 'could not start conformance-probe')

local deadline = now() + LIMIT_S
local next_frame = now()
local verdict
while now() < deadline and not verdict do
  if now() >= next_frame then
    next_frame = next_frame + 0.01
    host:frame(0.01)
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' and (e.ev == 'verdict' or e.ev == 'error') then
        verdict = e
      end
    end
  end
end
-- The probe stops its link and exits on its own; wait for it, so that no
-- process outlives the test.
local pid = (S.read(profile .. 'probe.pid') or ''):match('%d+')
local waited = now()
while pid and os.execute('kill -0 ' .. pid .. ' 2>/dev/null') == 0 do
  host:frame(0.01)
  if now() - waited > 20 then
    os.execute('kill ' .. pid)
    error('conformance-probe did not exit within 20 s of its verdict')
  end
end
local text = S.read(out) or ''
assert(verdict, 'no verdict within ' .. LIMIT_S .. ' s:\n' .. text:sub(-4000))
assert(verdict.ev == 'verdict' and verdict.pass, 'failed:\n' .. text:sub(-6000))
assert(verdict.mispredicted == 0, 'a command ended where RealSim did not predict')
assert(verdict.checks >= 9 and verdict.passed == verdict.checks, 'not every check ran and passed')
S.ok(string.format('RealSim passed %d conformance checks against the mock in %.1f s',
  verdict.passed, verdict.seconds))
