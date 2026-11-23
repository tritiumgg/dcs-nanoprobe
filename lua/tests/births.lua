-- The order of the births tier (spec/sampling.md, "Tiers"), with the real
-- shim and agent on the mock, and `link-probe --record` as the sidecar: the
-- pump resolves the oldest birth first, so the `OBJECT` records of units born
-- one after another come in the order of their births. Resolved newest
-- first, the first units of a long spawn waited for every later one
-- (docs/results/2026-11-17-bind-lag.md).

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/births.lua <module> <version> <link-probe>')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local OBJECT = 16
-- Units born in each of the waves, one group each.
local WAVE = 40
-- The births the shim's queue holds at most (spec/sampling.md, "The frame").
local MAX_BIRTHS = 4096

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local out = profile .. 'probe.jsonl'
local pidfile = profile .. 'probe.pid'
local recording = profile .. 'recording.jsonl'

-- A little-endian unsigned integer of `n` bytes at byte `off` of a hex string.
local function uint(hex, off, n)
  local v = 0
  for i = n - 1, 0, -1 do
    v = v * 256 + tonumber(hex:sub((off + i) * 2 + 1, (off + i) * 2 + 2), 16)
  end
  return v
end

-- The object IDs of the recording's OBJECT records that an event gave
-- (enumeration 0), in the order written.
local function born()
  local list = {}
  for _, line in ipairs(S.lines(recording)) do
    local ok, r = pcall(S.json, line)
    if ok and type(r) == 'table' and r.kind == OBJECT and uint(r.payload, 0x20, 8) == 0 then
      list[#list + 1] = uint(r.payload, 0, 8)
    end
  end
  return list
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
  -- No second enumeration during the test: every row below comes from a
  -- birth.
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 4096\nin_ring_kib = 256\n\n"
      .. "[sampling]\nenumeration_s = 600.0\n", map, port))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  assert(package.loadlib(version .. 'nanoprobe_agent_' .. BUILD .. '.dll', 'luaopen_nanoprobe'))

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('sampling')
  local sim = host.sim

  assert(os.execute(string.format(
    '%s --map %s --port %d --token-file %s --seconds 600 --record %s >> %s 2>&1 & echo $! > %s',
    S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(recording),
    S.quote(out), S.quote(pidfile))) == 0, 'could not start link-probe')

  -- Frames at about 100 a second until `done` is true, or `seconds` pass.
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

  assert(run(15, function()
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' and e.ev == 'hello' then
        return true
      end
    end
    return false
  end), 'the probe did not attach:\n' .. (S.read(out) or ''))
  -- The attach's enumeration ends before the test adds units.
  run(3)

  local usa = host.states.server.country.id.USA
  local want, mine = {}, {}
  local function wave(tag)
    for i = 1, WAVE do
      local name = string.format('np-b-%s-%02d', tag, i)
      sim:add_group(usa, 2, { name = name, units = { { name = name .. '-1', type = 'M-113',
        x = 42000 + 20 * #want, y = 0 } } })
      local id = sim.by_name[name .. '-1'].id
      want[#want + 1] = id
      mine[id] = true
    end
  end
  -- Two waves a frame apart, so births wait behind births: the second wave
  -- arrives while the first may still wait for a pump.
  wave('a')
  host:frame(0.01)
  wave('b')

  local function got()
    local list = {}
    for _, id in ipairs(born()) do
      if mine[id] then
        list[#list + 1] = id
      end
    end
    return list
  end
  -- The waits for OBJECT records end on the records; their 120 s only stops
  -- a hang. A loaded runner gives the agent's small share of each frame few
  -- frames, and 30 s was not enough there.
  assert(run(120, function()
    return #got() >= #want
  end), 'only ' .. #got() .. ' of ' .. #want .. ' births have an OBJECT record')
  local list = got()
  assert(#list == #want, #list .. ' OBJECT records for ' .. #want .. ' births')
  for i, id in ipairs(want) do
    assert(list[i] == id, string.format('OBJECT %d is object %d, wanted %d: births resolve oldest first',
      i, list[i], id))
  end
  S.ok(#want .. ' births in two waves resolve in the order they came, oldest first')

  -- The queue's bound and its restart. More births than the queue holds
  -- arrive in one frame: the mock delivers a frame's events together, before
  -- any pump can take one. Each names a unit that has its row, so resolving it
  -- costs one `wants` check and writes nothing. The count the shim hands
  -- `pump_begin` is the queue's length when a pump starts.
  local shim = host.states.server.__nanoprobe_shim
  local T, agent = shim.T, shim.agent
  local peak = 0
  local pump_begin = agent.pump_begin
  rawset(agent, 'pump_begin', function(births, ...)
    peak = math.max(peak, births)
    return pump_begin(births, ...)
  end)
  local known = sim.by_name['np-b-a-01-1']
  for _ = 1, MAX_BIRTHS + 104 do
    sim.queue[#sim.queue + 1] = { event = 'S_EVENT_BIRTH', initiator = known }
  end
  host:frame(0.01)
  local function waiting()
    return T.birth_tail - T.birth_head + 1
  end
  assert(waiting() <= MAX_BIRTHS, 'the queue holds ' .. waiting() .. ' births, over its bound of ' .. MAX_BIRTHS)
  assert(run(120, function()
    return waiting() == 0
  end), waiting() .. ' births still wait after 120 s')
  assert(peak == MAX_BIRTHS, 'the queue held ' .. peak .. ' births at most, not its bound of ' .. MAX_BIRTHS)
  assert(T.birth_head == 1 and T.birth_tail == 0,
    'an empty queue did not start over: head ' .. T.birth_head .. ', tail ' .. T.birth_tail)
  assert(next(T.births) == nil, 'an empty queue still holds an entry')
  -- A wave after the restart resolves in order, and the births of the unit
  -- that has its row wrote no second OBJECT.
  local before = #want
  wave('c')
  assert(run(120, function()
    return #got() >= #want
  end), 'only ' .. #got() .. ' of ' .. #want .. ' births have an OBJECT record after the queue emptied')
  list = got()
  assert(#list == #want, #list .. ' OBJECT records for ' .. #want .. ' births')
  for i = before + 1, #want do
    assert(list[i] == want[i], string.format('OBJECT %d is object %d, wanted %d: births after an empty queue',
      i, list[i], want[i]))
  end
  S.ok('the queue holds at most ' .. MAX_BIRTHS .. ' births, starts over when it empties, and resolves the '
    .. 'next wave in order')
end

local ok, err = pcall(main)
local pid = S.read(pidfile)
if pid then
  os.execute('kill ' .. pid:match('%d+') .. ' 2>/dev/null')
end
if not ok then
  io.stderr:write(tostring(err), '\n')
  io.stderr:write('probe output:\n', (S.read(out) or ''):sub(-3000), '\n')
  io.stderr:write('agent log:\n', (S.read(profile .. 'nanoprobe/logs/agent.log') or ''):sub(-2000), '\n')
  os.exit(1)
end
