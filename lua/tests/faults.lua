-- The rehearsal of the S8 fault tests (`cargo xtask faults`) on the host: the
-- hook and the shim in the strict mock, the host-native agent built with the
-- `faults` feature, and `link-probe` as the sidecar, over a real mapped file
-- and a real loopback handshake (spec/faults.md).
--
-- Each fault is survived or refused with a clear log line:
--
-- * a Lua error raised from the frame entry: the hook logs it, the next frame
--   pumps, the sidecar stays attached;
-- * a 1.5 s stall inside a frame: the sidecar stays attached;
-- * a flood of records past the ring and the event queue: the sidecar reads
--   a GAP and stays attached;
-- * a lost control record: the agent resyncs with a second HELLO;
-- * a stalled sidecar (SIGSTOP for 2 s): the agent detaches, and the sidecar
--   attaches again within 2 s of resuming, by wall time;
-- * a command shorter than its header, and five broken inbound rings: the
--   agent detaches naming the check that failed, and the sidecar attaches
--   again;
-- * a panic inside a frame: the agent is disabled, says why, and the hook
--   goes on without it.
--
-- Before them, inject refuses an unknown fault and arguments out of range.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/faults.lua <module> <version> <link-probe> <nanoprobe>')

local A
local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local pidfile = profile .. 'probe.pid'
local out
local probes = 0

local function stop_probe()
  local pid = (S.read(pidfile) or ''):match('%d+')
  if pid then
    os.execute('kill -CONT ' .. pid .. ' 2>/dev/null; kill ' .. pid .. ' 2>/dev/null')
  end
  os.remove(pidfile)
end

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

  local function start_probe(extra)
    stop_probe()
    probes = probes + 1
    out = profile .. 'probe' .. probes .. '.jsonl'
    assert(os.execute(string.format(
      '%s --map %s --port %d --token-file %s --seconds 120 %s > %s 2>&1 & echo $! > %s',
      S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), extra or '',
      S.quote(out), S.quote(pidfile))) == 0, 'could not start link-probe')
  end

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

  local function ev(name)
    return function(e)
      return e.ev == name
    end
  end

  local function log(target, msg)
    return function(e)
      return e.ev == 'log' and e.target == target and e.msg:sub(1, #msg) == msg
    end
  end

  local function dump()
    return '\n' .. (S.read(out) or '')
  end

  -- Wall milliseconds, from the probe's own clock.
  local function wall_ms()
    local pipe = io.popen(S.quote(PROBE) .. ' --now-ms')
    local text = pipe:read('*a')
    pipe:close()
    return assert(tonumber(text), 'no wall time from link-probe --now-ms')
  end

  local function answered()
    return find(function(e)
      return e.ev == 'result' and e.seq == 2 and e.code == 24
    end) ~= nil
  end

  -- Refusals: nil and why, and nothing armed.
  for _, bad in ipairs({ { 'nothing' }, { 'stall', 0 }, { 'stall', 10001 }, { 'flood', 0 }, { 'flood', 100001 } }) do
    local armed, why = A.inject(bad[1], bad[2])
    assert(armed == nil and type(why) == 'string' and why ~= '',
      'inject ' .. bad[1] .. ' ' .. tostring(bad[2]) .. ' gave ' .. tostring(armed) .. ', ' .. tostring(why))
  end
  S.ok('inject refuses an unknown fault and arguments out of range')

  start_probe()
  assert(run(10, answered), 'no attach and answered commands within 10 s:' .. dump())
  S.ok('the probe attaches and its commands are answered')

  -- A Lua error from the frame entry. The hook's guard catches it, and
  -- forwards its record to the sidecar.
  assert(A.inject('raise'))
  local frame = A.status().frame
  assert(run(2, function()
    return find(log('hook', 'onSimulationFrame raised')) ~= nil
  end), 'the sidecar did not read the raise:' .. dump())
  assert(run(1, function()
    return A.status().frame > frame + 5
  end), 'the agent did not pump after the raise')
  assert(count(ev('attached')) == 1, 'the probe attached again after the raise:' .. dump())
  local hooklog = S.read(profile .. 'nanoprobe/logs/hook.log') or ''
  assert(hooklog:find('injected Lua error', 1, true), 'hook.log does not hold the raised error')
  S.ok('a Lua error from the frame entry is logged, and the next frame pumps')

  -- A stall inside a frame, longer than the detach limit. The agent dates the
  -- heartbeat's change when it reads it, so it keeps the sidecar.
  assert(A.inject('stall', 1500))
  assert(run(4, function()
    return find(log('agent', 'injected fault: stall ended after ')) ~= nil
  end), 'the sidecar did not read the end of the stall:' .. dump())
  local stalled = tonumber(find(log('agent', 'injected fault: stall ended after ')).msg:match('after (%d+) ms'))
  assert(stalled and stalled >= 1500, 'the frame stalled ' .. tostring(stalled) .. ' ms, not 1500')
  run(1)
  assert(count(ev('attached')) == 1, 'the probe attached again after the stall:' .. dump())
  assert(not find(log('agent', 'detached')), 'the agent detached after its own stall:' .. dump())
  S.ok('a frame stalled ' .. stalled .. ' ms, and the sidecar stays attached')

  -- A flood: 3,000 records of about 1 KiB, past the 256 KiB ring and the
  -- 1 MiB event queue. Log records go first; the loss is a GAP.
  -- Only a GAP read after the flood counts: one may come earlier from what
  -- the agent queued before the sidecar attached. The flood's own record is
  -- among the oldest, so the overflow drops it too.
  local before = #events()
  assert(A.inject('flood', 3000))
  local function flood_gap()
    local list = events()
    for i = before + 1, #list do
      local e = list[i]
      if e.ev == 'gap' and e.class == 2 and e.count > 0 then
        return e
      end
    end
    return nil
  end
  assert(run(5, function()
    return flood_gap() ~= nil
  end), 'the sidecar read no log GAP after the flood:' .. dump())
  assert(count(ev('attached')) == 1, 'the probe attached again after the flood:' .. dump())
  S.ok('a flood past the ring and the queue is read as a GAP, attached throughout')

  -- A lost control record: the next LIFECYCLE is dropped, and the agent
  -- resyncs with a second HELLO in the same attach.
  assert(A.inject('lose-control'))
  host:load_mission('lost', { paused = true })
  assert(run(5, function()
    return count(ev('hello')) == 2
  end), 'no resync HELLO after a lost control record:' .. dump())
  assert(count(ev('attached')) == 1, 'the probe attached again for the resync:' .. dump())
  assert(find(log('agent', 'dropped a control record')), 'the sidecar did not read the drop:' .. dump())
  S.ok('a lost control record is logged and the agent resyncs with a HELLO')

  -- A stalled sidecar: stopped for 2 s, so its heartbeat stops. The agent
  -- detaches after 0.5 s; once resumed the sidecar attaches again within 2 s.
  local pid = assert((S.read(pidfile) or ''):match('%d+'), 'no probe pid')
  os.execute('kill -STOP ' .. pid)
  run(2)
  assert(not A.status().attached, 'the agent is still attached to a stopped sidecar')
  local resumed = wall_ms()
  os.execute('kill -CONT ' .. pid)
  -- The first attach after the resume.
  local function again()
    for _, e in ipairs(events()) do
      if e.ev == 'attached' and e.t >= resumed then
        return e
      end
    end
    return nil
  end
  assert(run(5, function()
    return again() ~= nil and find(log('agent', "detached: the sidecar's heartbeat stopped")) ~= nil
  end), 'the sidecar did not attach again and read the heartbeat detach:' .. dump())
  local took = again().t - resumed
  assert(took < 2000, 'the sidecar attached again ' .. took .. ' ms after resuming')
  S.ok('a stalled sidecar is detached and attaches again ' .. took .. ' ms after resuming')

  -- A command shorter than its header, from a new sidecar.
  start_probe('--short-command')
  assert(run(10, function()
    return count(ev('attached')) == 2
      and find(log('agent', 'a command too short for its header; detaching')) ~= nil
  end), 'no detach and new attach after the short command:' .. dump())
  S.ok('a command shorter than its header: the agent says so and detaches, and the sidecar attaches again')

  -- Five broken inbound rings. Each is a new sidecar; the agent detaches,
  -- says why, and the sidecar attaches again.
  -- What each broken ring must make the agent say (crates/transport/src/ring.rs).
  local EXPECT = {
    ['past-tail'] = 'record passes the tail',
    ['over-limit'] = 'len over an eighth of the ring',
    ['misaligned'] = 'not a multiple of 8',
    ['past-capacity'] = 'more than the capacity ahead of the head',
    ['padding'] = 'padding does not end at the end of the ring',
  }
  for _, bad in ipairs({ 'past-tail', 'over-limit', 'misaligned', 'past-capacity', 'padding' }) do
    start_probe('--bad-record ' .. bad)
    assert(run(10, function()
      return find(ev('injected')) ~= nil
    end), bad .. ': the probe did not break the ring:' .. dump())
    local injected = find(ev('injected'))
    assert(injected.what == bad, bad .. ': ' .. tostring(injected.what))
    assert(run(5, function()
      return count(ev('attached')) == 2 and find(log('agent', 'detached: inbound ring')) ~= nil
    end), bad .. ': no detach naming the inbound ring, and new attach:' .. dump())
    local said = find(log('agent', 'detached: inbound ring')).msg
    assert(said:find(EXPECT[bad], 1, true), bad .. ': the agent said ' .. said)
    S.ok(bad .. ': ' .. said .. '; the sidecar attaches again')
  end

  -- A panic inside a frame, holding the driver: the agent is disabled, sends
  -- its reason and a last LIFECYCLE, and the hook goes on without it.
  assert(A.inject('panic'))
  assert(run(3, function()
    return find(log('agent', 'disabled: a caught panic')) ~= nil
      and find(function(e)
        return e.ev == 'lifecycle' and e.state == 6
      end) ~= nil
  end), 'the sidecar did not read the panic\'s reason and DISABLED:' .. dump())
  assert(A.status().disabled, 'the agent is not disabled after the panic')
  run(0.5)
  hooklog = S.read(profile .. 'nanoprobe/logs/hook.log') or ''
  assert(hooklog:find('the agent is disabled; the hook goes on without it', 1, true),
    'hook.log does not say the agent is disabled')
  local agentlog = S.read(profile .. 'nanoprobe/logs/agent.log') or ''
  assert(agentlog:find('disabled after a panic', 1, true), 'agent.log does not record the panic')
  assert(agentlog:find('injected fault: panic', 1, true), 'agent.log does not say the panic was injected')
  S.ok('a panic in a frame disables the agent with its reason, and the hook goes on')
end

local ok, err = pcall(main)
stop_probe()
if not ok and out then
  io.stderr:write(S.read(out) or '', '\n')
end
S.remove(profile)
assert(ok, err)
