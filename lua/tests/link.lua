-- The rehearsal of the S7 in-game test (`cargo xtask link`) on the host: the
-- hook and the shim in the strict mock, the host-native agent, `link-probe` as
-- the sidecar and `nanoprobe inspect`, over a real mapped file and a real
-- loopback handshake.
--
-- Frames run at about 100 a second. Inside each mission load, after
-- onMissionLoadBegin, 0.5 s passes with no frame, as in DCS. The steps are the
-- live test's: the handshake seen in the inspector, ten reloads each read by
-- the sidecar under 2 s after the mission runs, a shim reload, and the kill
-- switch. The rehearsal also sends one command shorter than its header, which
-- the live test does not.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE, INSPECT = ...
assert(MODULE and PROBE and INSPECT,
  'usage: lua lua/tests/link.lua <module> <version> <link-probe> <nanoprobe>')

-- The agent's table in this Lua state, opened after the install from the
-- same file the hook opens: on Linux another path would load another copy,
-- with state of its own.
local A

-- Seconds, by CPU time. Every wait here spins, so it keeps close to real
-- time, and it still runs after the kill switch stops every agent call.
local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local RELOADS = 10

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
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 256\nin_ring_kib = 256\n", map, port))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  A = assert(package.loadlib(version .. 'nanoprobe_agent_' .. BUILD .. '.dll', 'luaopen_nanoprobe'))()

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('first', { paused = true })

  -- The probe lives longer than every wait below added up.
  assert(os.execute(string.format(
    '%s --map %s --port %d --token-file %s --seconds 120 --short-command > %s 2>&1 & echo $! > %s',
    S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(out),
    S.quote(pidfile))) == 0, 'could not start link-probe')

  local function events()
    local list = {}
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' then
        list[#list + 1] = e
      end
    end
    return list
  end

  local function find(pred)
    for _, e in ipairs(events()) do
      if pred(e) then
        return e
      end
    end
    return nil
  end

  local function count(pred)
    local n = 0
    for _, e in ipairs(events()) do
      if pred(e) then
        n = n + 1
      end
    end
    return n
  end

  -- Frames at about 100 a second until `done`, or `seconds`.
  local function run(seconds, done)
    local deadline = now() + seconds
    local next_frame = now()
    while now() < deadline do
      if now() >= next_frame then
        next_frame = next_frame + 0.01
        host:frame(0.01)
        if done and done() then
          return true
        end
      end
    end
    return done == nil
  end

  local function spin(seconds)
    local deadline = now() + seconds
    while now() < deadline do
    end
  end

  local function attached(e)
    return e.ev == 'attached'
  end

  -- The handshake: the probe attaches, reads the HELLO, and announces
  -- campaign 7. Its command for campaign 7 is answered UNKNOWN_OPCODE (20),
  -- and its command for campaign 8 WRONG_CAMPAIGN (24).
  local function result(seq, code)
    return find(function(e)
      return e.ev == 'result' and e.seq == seq and e.code == code
    end) ~= nil
  end
  assert(run(10, function()
    return result(1, 20) and result(2, 24)
  end), 'no attach and answered commands within 10 s:\n' .. (S.read(out) or ''))
  local hello = find(function(e)
    return e.ev == 'hello'
  end)
  assert(hello and hello.interface == 2, 'the HELLO reads interface ' .. tostring(hello and hello.interface))
  assert(hello.build == BUILD, 'the HELLO reads build ' .. tostring(hello.build))
  assert(find(function(e)
    return e.ev == 'log' and e.target == 'hook' and e.msg == 'capabilities'
  end), 'the hook capabilities did not reach the sidecar')
  assert(find(function(e)
    return e.ev == 'log' and e.target == 'shim' and e.msg == 'capabilities'
  end), 'the shim capabilities did not reach the sidecar')
  S.ok('the probe attaches, reads the HELLO and the logs, and CAMPAIGN decides how its commands are answered')

  -- The command shorter than its header: the agent says so to the sidecar and
  -- detaches, and the probe attaches again.
  assert(run(10, function()
    return count(attached) == 2 and count(function(e)
      return e.ev == 'hello'
    end) == 2
  end), 'no detach and new attach after the short command:\n' .. (S.read(out) or ''))
  assert(find(function(e)
    return e.ev == 'log' and e.target == 'agent' and e.msg == 'a command too short for its header; detaching'
  end), 'the sidecar did not read why the agent detached')
  S.ok('a command shorter than its header is logged to the sidecar and the agent detaches')

  -- The inspector shows the handshake while attached.
  local pipe = io.popen(S.quote(INSPECT) .. ' inspect ' .. S.quote(map) .. ' --records 5 2>&1')
  local inspect = pipe:read('*a')
  pipe:close()
  assert(inspect:find('ATTACHED', 1, true), 'the inspector does not show ATTACHED:\n' .. inspect)
  S.ok('nanoprobe inspect shows the agent attached')

  -- Ten reloads, each with 0.5 s with no frame inside the load. The sidecar
  -- reads LOADING during that time, with no mission named. STOPPING names
  -- the mission that stops, and the new mission's PAUSED names the new one.
  for i = 1, RELOADS do
    local loading_read = false
    local old_gen = A.status().gen
    local old_mission = i == 1 and 'first.miz' or ('reload ' .. (i - 1) .. '.miz')
    host:load_mission('reload ' .. i, { paused = true, during = function()
      local gen = A.status().gen
      local deadline = now() + 0.5
      while now() < deadline do
        loading_read = loading_read or find(function(e)
          return e.ev == 'lifecycle' and e.gen == gen and e.state == 2
            and e.theater == '' and e.mission == ''
        end) ~= nil
        spin(0.05)
      end
    end })
    assert(loading_read, 'reload ' .. i .. ': the sidecar did not read LOADING, unnamed, during the load')
    assert(find(function(e)
      return e.ev == 'lifecycle' and e.gen == old_gen and e.state == 5
        and e.theater == 'Caucasus' and e.mission == old_mission
    end), 'reload ' .. i .. ': no STOPPING naming ' .. old_mission)
    local gen = A.status().gen
    assert(run(3, function()
      return find(function(e)
        return e.ev == 'lifecycle' and e.gen == gen and e.state == 4
          and e.theater == 'Caucasus' and e.mission == 'reload ' .. i .. '.miz'
      end) ~= nil
    end), 'reload ' .. i .. ': the sidecar did not read generation ' .. gen .. ' paused')
  end
  assert(count(attached) == 2, 'the probe attached again during the reloads')
  S.ok(RELOADS .. ' reloads, each read by the sidecar with its names, attached throughout')

  -- A shim reload request, seen by the sidecar through the hook's record.
  S.write(profile .. 'nanoprobe/reload', '')
  assert(run(3, function()
    return find(function(e)
      return e.ev == 'log' and e.target == 'hook' and e.msg == 'shim reloaded'
    end) ~= nil
  end), 'the sidecar did not read shim reloaded')
  S.ok('a shim reload request reaches the sidecar as shim reloaded')

  -- The kill switch: the agent stores DISABLED, sends its reason and a last
  -- LIFECYCLE, and stops.
  S.write(profile .. 'nanoprobe/disable', '')
  assert(run(2, function()
    return find(function(e)
      return e.ev == 'status' and e.lifecycle == 6
    end) ~= nil
  end), 'the status block did not read DISABLED within 2 s')
  local frame = A.status().frame
  run(0.2)
  assert(A.status().frame == frame, 'the agent pumped after the kill switch')

  -- The probe stops a second after DISABLED; wait for its summary.
  local deadline = now() + 30
  while not find(function(e)
    return e.ev == 'end'
  end) and now() < deadline do
    spin(0.1)
  end
  local reason = find(function(e)
    return e.ev == 'log' and e.target == 'agent' and e.msg == 'disabled: the kill switch'
  end)
  local last = find(function(e)
    return e.ev == 'lifecycle' and e.state == 6
  end)
  assert(reason and last and reason.t <= last.t, 'the reason and the last LIFECYCLE did not reach the sidecar')
  assert(last.theater == 'Caucasus' and last.mission == 'reload ' .. RELOADS .. '.miz',
    'DISABLED names ' .. tostring(last.theater) .. ', ' .. tostring(last.mission))
  S.ok('the kill switch sends its reason and a last LIFECYCLE, stores DISABLED, and the pump stops')

  -- Command time is counted: three commands on each attach. The short one
  -- has no time. No attach after the kill switch: the listener is gone.
  local final = find(function(e)
    return e.ev == 'final'
  end)
  assert(final and final.commands == 7 and final.timed == 6,
    'the status block counts ' .. tostring(final and final.commands) .. ' commands, '
    .. tostring(final and final.timed) .. ' timed')
  assert(count(attached) == 2, 'the probe attached ' .. count(attached) .. ' times')
  local worst, reloads = 0, 0
  for _, e in ipairs(events()) do
    if e.ev == 'reconnect' and e.gen >= 2 then
      reloads = reloads + 1
      worst = math.max(worst, e.ms)
    end
  end
  assert(reloads == RELOADS, 'reconnect lines for ' .. reloads .. ' reloads, not ' .. RELOADS)
  assert(worst < 2000, 'a reload took ' .. worst .. ' ms to reach the sidecar')
  S.ok('every reload reached the sidecar in under 2 s; the slowest took ' .. worst .. ' ms')
end

local ok, err = pcall(main)
-- Whatever happened, stop the probe and remove the profile.
local pid = (S.read(pidfile) or ''):match('%d+')
if pid then
  os.execute('kill ' .. pid .. ' 2>/dev/null')
end
if not ok then
  io.stderr:write(S.read(out) or '', '\n')
end
S.remove(profile)
assert(ok, err)
