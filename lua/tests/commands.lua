-- The rehearsal of the S14 in-game test on the host: the hook and the shim in
-- the strict mock with raw Lua on, the host-native agent, and
-- `command-probe` as the sidecar, over a real mapped file and a real
-- loopback handshake. The probe writes seeded mixed commands, extension
-- registrations (set-state) and invocations (effects), restarting itself as
-- a sidecar every 100 commands (the numbers `xtask commands` uses on the
-- server), and audits what the shim ran. The mock runs
-- at about 100 frames a second; the mission is paused, as on the server.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/commands.lua <module> <version> <link-probe>')
local COMMAND_PROBE = PROBE:gsub('link%-probe$', 'command-probe')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local out = profile .. 'probe.jsonl'
local pidfile = profile .. 'probe.pid'

local function main()
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
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 256\nin_ring_kib = 256\n\n[lua]\nraw = true\n",
    map, port))
  S.write(profile .. 'nanoprobe/token', TOKEN)

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('first', { paused = true })

  assert(os.execute(string.format(
    '%s --map %s --port %d --token-file %s --commands 1000 --restart-every 100 --seconds 280 > %s 2>&1 & echo $! > %s',
    S.quote(COMMAND_PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(out),
    S.quote(pidfile))) == 0, 'could not start command-probe')

  local function verdict()
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' and (e.ev == 'verdict' or e.ev == 'error') then
        return e
      end
    end
    return nil
  end

  local deadline = now() + 290
  local next_frame = now()
  local v
  while now() < deadline do
    if now() >= next_frame then
      next_frame = next_frame + 0.01
      host:frame(0.01)
      v = verdict()
      if v then
        break
      end
    end
  end
  assert(v, 'no verdict within 290 s:\n' .. (S.read(out) or ''))
  assert(v.ev == 'verdict' and v.pass, 'the run failed:\n' .. (S.read(out) or ''))
  assert(v.restarts >= 9, 'only ' .. tostring(v.restarts) .. ' sidecar restarts')
  assert(v.cutting_restarts > 0, 'no sidecar restart cut a command in flight')
  S.ok(string.format('%d commands, %d sidecar restarts: %d revisions run once each, %d effects run once each',
    v.written, v.restarts, v.set_runs, v.invokes))
end

local ok, err = pcall(main)
local pid = (S.read(pidfile) or ''):match('%d+')
if pid then
  os.execute('kill ' .. pid .. ' 2>/dev/null')
end
if not ok then
  io.stderr:write(S.read(out) or '', '\n')
end
S.remove(profile)
assert(ok, err)
