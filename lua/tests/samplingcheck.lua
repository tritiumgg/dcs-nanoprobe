-- The shim's existence check (`check` in lua/agent/shim.lua) driven into the
-- agent's `alive` entry, with the real shim and agent on the mock, and
-- `link-probe --record` as the sidecar. The shim tells the agent whether a
-- row survived (isExist true), or whether the check could not decide (the
-- call raised, or the shim holds no object). A wrong flag changes what the
-- sampler does with a death-event mark and with an enumeration's miss:
--
--   a. a mark on a row whose isExist is true is cleared, and counted in the
--      LOG key `marks_cleared`;
--   b. a mark on a row whose isExist raises stays, `marks_cleared` does not
--      move, and when the unit then goes, a later check writes DEAD;
--   c. an enumeration's miss whose isExist raises gets an OBJECT with that
--      enumeration's number before ENUM_END, and no OBJECT_GONE.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/samplingcheck.lua <module> <version> <link-probe>')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local OBJECT, OBJECT_GONE, ENUM_BEGIN, ENUM_END = 16, 17, 18, 19
local DEAD = 1

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

-- The recording's OBJECT, OBJECT_GONE, ENUM_BEGIN and ENUM_END records, in
-- order, as tables.
local function records()
  local list = {}
  for _, line in ipairs(S.lines(recording)) do
    local ok, r = pcall(S.json, line)
    if ok and type(r) == 'table' then
      local k, p = r.kind, r.payload
      if k == OBJECT then
        list[#list + 1] = { kind = 'object', id = uint(p, 0, 8), enumeration = uint(p, 0x20, 8) }
      elseif k == OBJECT_GONE then
        list[#list + 1] = { kind = 'gone', id = uint(p, 0, 8), reason = uint(p, 8, 4) }
      elseif k == ENUM_BEGIN then
        list[#list + 1] = { kind = 'begin', enumeration = uint(p, 0, 8) }
      elseif k == ENUM_END then
        list[#list + 1] = { kind = 'end', enumeration = uint(p, 0, 8) }
      end
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
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 4096\nin_ring_kib = 256\n\n"
      .. "[sampling]\nenumeration_s = 5.0\n", map, port))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  assert(package.loadlib(version .. 'nanoprobe_agent_' .. BUILD .. '.dll', 'luaopen_nanoprobe'))

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('sampling')
  local sim = host.sim
  sim.exist_raises = {}

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

  -- The newest `sampling` report in the agent's log: its count of reports
  -- and its marks_cleared.
  local function report()
    local count, cleared = 0, nil
    for _, line in ipairs(S.lines(profile .. 'nanoprobe/logs/agent.log')) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' and e.msg == 'sampling' and type(e.data) == 'table' then
        count = count + 1
        cleared = e.data.marks_cleared
      end
    end
    return count, cleared
  end
  -- Run until the agent has written `more` sampling reports after this call.
  local function next_report(more)
    local count = report()
    assert(run(30, function()
      return (report()) >= count + more
    end), 'no sampling report in the agent log')
    local _, cleared = report()
    assert(type(cleared) == 'number', 'the sampling report has no marks_cleared')
    return cleared
  end

  local usa = host.states.server.country.id.USA
  local function add(name, x)
    sim:add_group(usa, 2, { name = name, units = { { name = name .. '-1', type = 'M-113', x = x, y = 0 } } })
    return sim.by_name[name .. '-1']
  end
  local a, b, c = add('np-chk-a', 40000), add('np-chk-b', 40100), add('np-chk-c', 40200)
  -- Births resolve, an enumeration finishes, and the rows are held.
  run(12)
  local base = next_report(1)

  local function count_gone(id)
    local n = 0
    for _, r in ipairs(records()) do
      if r.kind == 'gone' and r.id == id then
        n = n + 1
      end
    end
    return n
  end
  assert(count_gone(a.id) == 0 and count_gone(b.id) == 0 and count_gone(c.id) == 0, 'a row left before the test')

  -- (a) A death event on a unit that exists: the row is marked, its check
  -- finds isExist true, the mark is cleared and counted, the row stays.
  sim.queue[#sim.queue + 1] = { event = 'S_EVENT_DEAD', initiator = a }
  run(1)
  local cleared = next_report(2)
  assert(cleared == base + 1, 'marks_cleared ' .. tostring(cleared) .. ', wanted ' .. (base + 1))
  assert(count_gone(a.id) == 0, 'a death event on a live unit removed its row')
  S.ok('a mark on a row whose isExist is true is cleared and counted in marks_cleared')

  -- (b) The same event on a unit whose isExist raises: the check cannot
  -- decide, so the mark stays and nothing is counted.
  sim.exist_raises[b.id] = true
  sim.queue[#sim.queue + 1] = { event = 'S_EVENT_DEAD', initiator = b }
  run(1)
  cleared = next_report(2)
  assert(cleared == base + 1, 'marks_cleared moved to ' .. tostring(cleared) .. ' for a check that raised')
  assert(count_gone(b.id) == 0, 'a check that raised removed the row')
  -- The unit goes with no event, and isExist answers again. A later check
  -- finds it gone and the kept mark makes it DEAD, where an unmarked row
  -- removed this way reads REMOVED.
  sim.exist_raises[b.id] = nil
  sim:remove_unit(b)
  assert(run(40, function()
    return count_gone(b.id) >= 1
  end), 'no later check found the marked unit gone')
  local gone
  for _, r in ipairs(records()) do
    if r.kind == 'gone' and r.id == b.id then
      gone = gone or r
    end
  end
  assert(gone.reason == DEAD, 'the marked unit left with reason ' .. gone.reason .. ', wanted DEAD')
  assert(next_report(1) == base + 1, 'marks_cleared moved when the marked row left')
  S.ok('a mark on a row whose isExist raises stays, and a later check that finds it gone gives DEAD')

  -- (c) A unit goes while isExist raises for it. An enumeration that starts
  -- after that misses it, its check cannot decide, and the miss gets an
  -- OBJECT with that enumeration's number before ENUM_END, and no
  -- OBJECT_GONE.
  sim.exist_raises[c.id] = true
  sim:remove_unit(c)
  local seen = #records()
  local number
  assert(run(40, function()
    local list = records()
    for i = seen + 1, #list do
      local r = list[i]
      if r.kind == 'begin' and not number then
        number = r.enumeration
      elseif r.kind == 'end' and number and r.enumeration == number then
        return true
      end
    end
    return false
  end), 'no enumeration began and ended after the unit went')
  local list = records()
  local at, finish
  for i = seen + 1, #list do
    local r = list[i]
    if r.kind == 'object' and r.id == c.id and r.enumeration == number then
      at = at or i
    elseif r.kind == 'end' and r.enumeration == number then
      finish = i
    elseif r.kind == 'gone' and r.id == c.id then
      error('a check that raised wrote OBJECT_GONE for the unit')
    end
  end
  assert(at, 'no OBJECT with enumeration ' .. number .. ' for the miss whose isExist raises')
  assert(finish and at < finish, 'the OBJECT for the miss does not come before ENUM_END')
  S.ok('an enumeration miss whose isExist raises gets an OBJECT with its enumeration number before ENUM_END')
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
