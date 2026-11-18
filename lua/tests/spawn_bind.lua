-- The rows of a spawned group's units (spec/commands.md, "Layout:
-- GROUP_SPAWN"; spec/records.md, "Layout: OBJECT", "Layout: OBJECT_GONE",
-- "Layout: RESULT"), with the real shim and the real agent on the mock and
-- `command-script` as the sidecar. One `GROUP_SPAWN` handler reads each new
-- unit as the births tier would, so the agent writes the units' `OBJECT`
-- records inside the command, before its `RESULT`.
--
-- The recording tools cannot do this alone: `link-probe --record` sends no
-- `GROUP_SPAWN`, and `command-script` prints the `RESULT` and `OBJECT_GONE`
-- records but not `OBJECT`. This test therefore wraps the agent's own
-- functions in the server state (`__nanoprobe_shim.agent`): `object`,
-- `command_done`, `pump_begin` and `event`. Each wrapper logs the call and
-- calls the real function, which writes the record. The agent writes an
-- `OBJECT` record in `agent.object` and the `OBJECT_GONE` records and the
-- `RESULT` in `agent.command_done`, so the log's order is the order of those
-- records in the stream. `command-script` supplies the records the wrappers
-- cannot see (`RESULT` with its code and `cost_us`, `OBJECT_GONE` with its
-- reason), in the order read. A `pump_begin` call starts a frame's pump, so
-- the log says which frame each call ran in (the stream's own `FRAME`
-- records are not visible to `command-script`).
--
-- One script holds every spawn, because a second attach would enumerate
-- every object again:
--   bind:    a group of 4 units
--   replace: the same name again, 1.5 s later (about 150 frames)
--   raise:   a group of 3 units, the middle one's first `getTypeName` raises
--   slow:    a group whose first read takes 8 ms, over the 3 ms budget
--   marker:  a group that reads the over-budget count after `slow`
-- The mock queues a spawned unit's `S_EVENT_BIRTH` and delivers it on a later
-- frame, after the handler's reads (UNVERIFIED in DCS: the mock's choice).

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/spawn_bind.lua <module> <version> <link-probe>')
local COMMAND_SCRIPT = PROBE:gsub('link%-probe$', 'command-script')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local APPLIED = 0
local DESTROYED = 2
local BUDGET_MS = 3.0
local SLOW_MS = 8
local USA = 2

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local out = profile .. 'script.jsonl'
local pidfile = profile .. 'script.pid'

local function q(text)
  return '"' .. text:gsub('[\\"]', '\\%0'):gsub('\n', '\\n') .. '"'
end

-- A Lua value as JSON, for command-script. A number key is written `#<n>`.
local function json(v)
  if type(v) == 'table' then
    local parts = {}
    if #v > 0 then
      for _, x in ipairs(v) do
        parts[#parts + 1] = json(x)
      end
      return '[' .. table.concat(parts, ',') .. ']'
    end
    local keys = {}
    for k in pairs(v) do
      keys[#keys + 1] = k
    end
    table.sort(keys, function(a, b)
      return tostring(a) < tostring(b)
    end)
    for _, k in ipairs(keys) do
      parts[#parts + 1] = q((type(k) == 'number' and '#' or '') .. tostring(k)) .. ':' .. json(v[k])
    end
    return '{' .. table.concat(parts, ',') .. '}'
  elseif type(v) == 'string' then
    return q(v)
  end
  return tostring(v)
end

local function group(name, units)
  local list = {}
  for i, u in ipairs(units) do
    list[i] = { name = u, type = 'M-113', x = 1000 + 10 * i, y = 2000, heading = 0 }
  end
  return { name = name, units = list }
end

-- The status block's `commands_over_budget` (spec/transport.md, offset
-- 0x0128, a little-endian u64), read from the mapped file.
local function over_budget()
  local f = assert(io.open(map, 'rb'))
  f:seek('set', 0x128)
  local bytes = f:read(8)
  f:close()
  local v = 0
  for i = 8, 1, -1 do
    v = v * 256 + bytes:byte(i)
  end
  return v
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
  -- No second enumeration during the test: every OBJECT below comes from a
  -- spawn or a birth.
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 4096\nin_ring_kib = 256\n\n"
      .. "[sampling]\nenumeration_s = 600.0\n\n[budget]\ncommanded_ms = %s\n", map, port, BUDGET_MS))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  assert(package.loadlib(version .. 'nanoprobe_agent_' .. BUILD .. '.dll', 'luaopen_nanoprobe'))

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('first', { paused = false })
  local server = host.states.server
  local agent = server.__nanoprobe_shim.agent
  local Unit = server.Unit

  -- The log of the agent calls that write records, in order.
  local log, frame, hostframe = {}, 0, 0
  -- `wants` is the check a read starts with: true when the agent has no row
  -- for the ID and takes one. Its answer is logged with the call.
  local real_wants = agent.wants
  rawset(agent, 'wants', function(id, ...)
    local wanted, died = real_wants(id, ...)
    log[#log + 1] = { ev = 'wants', id = id, wanted = wanted, frame = frame, host = hostframe }
    return wanted, died
  end)
  for _, name in ipairs({ 'pump_begin', 'object', 'command_done', 'event' }) do
    local real = agent[name]
    rawset(agent, name, function(...)
      if name == 'pump_begin' then
        frame = frame + 1
      elseif name == 'object' then
        local a = { ... }
        log[#log + 1] = { ev = 'object', id = a[1], name = a[8], enumerated = a[11], frame = frame, host = hostframe }
      elseif name == 'command_done' then
        local a = { ... }
        log[#log + 1] = { ev = 'done', code = a[1], destroyed = a[4], frame = frame, host = hostframe }
      else
        log[#log + 1] = { ev = 'event', frame = frame }
      end
      return real(...)
    end)
  end

  -- Getter wrappers: the first `getTypeName` of `np-r-2` raises, and the
  -- first of `np-s-1` takes SLOW_MS. The over-budget count is read at the
  -- first read of `np-s-1` and of `np-m-1`, so only the slow command lies
  -- between the two readings.
  local real_type, real_name = Unit.getTypeName, Unit.getName
  local fired, over_at = {}, {}
  local function once(name)
    if fired[name] then
      return false
    end
    fired[name] = true
    return true
  end
  rawset(Unit, 'getTypeName', function(self, ...)
    local name = real_name(self)
    if name == 'np-r-2' and once(name) then
      error('a getter that raises, for the test', 0)
    elseif name == 'np-s-1' and once(name) then
      over_at.before = over_budget()
      local stop = now() + SLOW_MS / 1000
      while now() < stop do
      end
    elseif name == 'np-m-1' and once(name) then
      over_at.after = over_budget()
    end
    return real_type(self, ...)
  end)
  assert(over_budget() == 0, 'the over-budget count is ' .. over_budget() .. ' before any command')

  local gen = agent.status().gen
  -- The agent names a spawned group from the campaign and the command's
  -- entity (`np!<tag><entity>`, spec/commands.md), and the handler looks a
  -- group up by that name: the group's own name must be it.
  local G1, G3, G4, G5 = 'np!hGx1', 'np!hGx2', 'np!hGx3', 'np!hGx4'
  local U1 = { 'np-b-1', 'np-b-2', 'np-b-3', 'np-b-4' }
  local U2 = { 'np-b-5', 'np-b-6', 'np-b-7', 'np-b-8' }
  local U3 = { 'np-r-1', 'np-r-2', 'np-r-3' }
  local U4 = { 'np-s-1', 'np-s-2' }
  local U5 = { 'np-m-1' }
  local function spawn(entity, rev, name, units, extra)
    local c = { op = 'GROUP_SPAWN', entity = entity, rev = rev, gen = gen, country = USA, category = 2,
      name = name, group = group(name, units), wait_result = true }
    for k, v in pairs(extra or {}) do
      c[k] = v
    end
    return c
  end
  S.write(profile .. 'script.json', json({ campaign = 7, commands = {
    spawn(1, 1, G1, U1),
    spawn(1, 2, G1, U2, { wait_ms = 1500 }),
    spawn(2, 1, G3, U3),
    spawn(3, 1, G4, U4),
    spawn(4, 1, G5, U5),
  } }))
  assert(os.execute(string.format(
    '%s --map %s --port %d --token-file %s --script %s --seconds 60 --linger-ms 300 > %s 2>&1 & echo $! > %s',
    S.quote(COMMAND_SCRIPT), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'),
    S.quote(profile .. 'script.json'), S.quote(out), S.quote(pidfile))) == 0, 'could not start command-script')

  local deadline = now() + 40
  local next_frame = now()
  local ended
  while now() < deadline and not ended do
    if now() >= next_frame then
      next_frame = next_frame + 0.01
      hostframe = hostframe + 1
      host:frame(0.01)
      for _, e in ipairs(events()) do
        if e.ev == 'end' then
          ended = e
        end
      end
    end
  end
  assert(ended and ended.complete, 'the script did not finish:\n' .. (S.read(out) or ''))
  -- Many more frames: the births reach the births tier, every pump runs.
  for _ = 1, 60 do
    host:frame(0.01)
  end

  -- The script's records, in the order read, and the results by seq.
  local ev = events()
  local result = {}
  for _, e in ipairs(ev) do
    if e.ev == 'result' then
      result[e.seq] = e
    end
  end
  local function expect_applied(seq, what)
    local r = result[seq]
    assert(r and r.code == APPLIED and r.code_name == 'APPLIED',
      what .. ': answered ' .. tostring(r and r.code_name) .. ' ' .. tostring(r and r.message))
    return r
  end

  -- The log, split at each `done`: the calls since the `done` before it. A
  -- split can also hold a row the births tier wrote for another group's unit
  -- (the unit whose read raised gets its row at its birth, in whichever frame
  -- the pump resolves it), so `own` below keeps a command's own units.
  local commands, current = {}, { objects = {}, calls = {} }
  for _, l in ipairs(log) do
    if l.ev == 'object' and l.name:find('^np%-') then
      current.objects[#current.objects + 1] = l
      current.calls[#current.calls + 1] = l
    elseif l.ev == 'event' then
      current.calls[#current.calls + 1] = l
    elseif l.ev == 'done' then
      current.done = l
      commands[#commands + 1] = current
      current = { objects = {}, calls = {} }
    end
  end
  -- The `done` calls of the spawns, in order: the script's five commands are
  -- the only commands the agent ran for the sidecar's `seq` 1 to 5.
  local spawns = {}
  for _, c in ipairs(commands) do
    if #c.objects > 0 or c.done.code == APPLIED then
      spawns[#spawns + 1] = c
    end
  end
  assert(#spawns >= 5, 'the log holds ' .. #spawns .. ' commands, not 5')
  local first = #spawns - 4

  local function names(list)
    local set = {}
    for _, n in ipairs(list) do
      set[n] = true
    end
    return set
  end
  -- Command `n` of the script with the rows of its own `units`, and the
  -- events that reached the agent between the first of those rows and the
  -- RESULT. A row of another unit in the split fails the test, unless it is
  -- `late`, the unit a birth may give its row at any time.
  local function command(n, units, late)
    local c, set = spawns[first + n - 1], names(units)
    local objects, between = {}, 0
    for _, l in ipairs(c.calls) do
      if l.ev == 'object' and set[l.name] then
        objects[#objects + 1] = l
      elseif l.ev == 'object' then
        assert(l.name == late, 'command ' .. n .. ' wrote an OBJECT for ' .. l.name)
      elseif l.ev == 'event' and #objects > 0 then
        between = between + 1
      end
    end
    c.objects, c.events = objects, between
    return c
  end
  local function count(list, set)
    local n = {}
    for _, o in ipairs(list) do
      if set[o.name] then
        n[o.name] = (n[o.name] or 0) + 1
      end
    end
    return n
  end

  -- 1. Order: every new unit's OBJECT before the RESULT, in its frame.
  local c1 = command(1, U1)
  expect_applied(1, 'the first spawn')
  assert(#c1.objects == #U1, #c1.objects .. ' OBJECT records before the RESULT, not ' .. #U1)
  local set1 = names(U1)
  for _, o in ipairs(c1.objects) do
    assert(set1[o.name], 'an OBJECT for ' .. o.name .. ' in the spawn of ' .. G1)
    assert(o.enumerated == false, o.name .. ' is an enumeration row, not an event row')
    assert(o.frame == c1.done.frame, o.name .. ' in frame ' .. o.frame .. ', the RESULT in ' .. c1.done.frame)
  end
  S.ok('a spawn of ' .. #U1 .. ' units writes ' .. #c1.objects
    .. ' OBJECT records before its RESULT (APPLIED), in the RESULT\'s frame')

  -- 2. No second OBJECT: over the whole run, each unit of the first spawn has
  -- exactly one.
  local set2, set3 = names(U2), names(U3)
  local all = {}
  for _, l in ipairs(log) do
    if l.ev == 'object' then
      all[#all + 1] = l
    end
  end
  local n1 = count(all, set1)
  for _, u in ipairs(U1) do
    assert(n1[u] == 1, u .. ' has ' .. tostring(n1[u]) .. ' OBJECT records, not 1')
  end
  local c2 = command(2, U2)
  -- The births were resolved, and each cost one `wants` check: after a
  -- spawn's RESULT the agent is asked about each of its units again, by the
  -- births tier, and answers that it wants no row. The frames between the
  -- spawns are not counted: their number depends on the machine's load.
  local function checked_after(c, what)
    local at
    for i, l in ipairs(log) do
      if l == c.done then
        at = i
      end
    end
    local asked = {}
    for i = at + 1, #log do
      local l = log[i]
      if l.ev == 'wants' then
        asked[l.id] = asked[l.id] or {}
        asked[l.id][#asked[l.id] + 1] = l.wanted
      end
    end
    for _, o in ipairs(c.objects) do
      local answers = asked[o.id]
      assert(answers, what .. ': the birth of ' .. o.name .. ' never reached the births tier')
      -- One birth event for each unit, and no enumeration in the test (it
      -- asks with `seen`, not `wants`): one check.
      assert(#answers == 1, what .. ': ' .. o.name .. ' cost ' .. #answers .. ' wants checks, not 1')
      assert(answers[1] == false, what .. ': the agent wanted a second row for ' .. o.name)
    end
  end
  checked_after(c1, 'the first spawn')
  S.ok('each of the ' .. #U1 .. ' units has exactly one OBJECT record, and its birth cost the births tier '
    .. 'one wants check that took no row')

  -- 3. Replace: new OBJECTs, then one OBJECT_GONE (DESTROYED) per old unit,
  -- then the RESULT.
  expect_applied(2, 'the replacing spawn')
  assert(#c2.objects == #U2, #c2.objects .. ' OBJECT records in the replace, not ' .. #U2)
  for _, o in ipairs(c2.objects) do
    assert(set2[o.name] and o.frame == c2.done.frame, 'the replace wrote an OBJECT for ' .. o.name)
  end
  local old_ids = {}
  for _, o in ipairs(c1.objects) do
    old_ids[o.id] = o.name
  end
  -- The agent writes the old rows' OBJECT_GONE inside `command_done`, from
  -- the IDs the handler hands it. That call is logged after the replace's
  -- `object` calls (they are in `c2.objects`), so the IDs it carries fix the
  -- order: the new OBJECTs, then the old units' OBJECT_GONE, then the RESULT.
  assert(c1.done.destroyed == nil or #c1.done.destroyed == 0, 'the first spawn destroyed something')
  local handed = c2.done.destroyed
  assert(type(handed) == 'table' and #handed == #U1,
    'the replace handed command_done ' .. (type(handed) == 'table' and #handed or 0) .. ' destroyed IDs, not ' .. #U1)
  for _, id in ipairs(handed) do
    assert(old_ids[id], 'the replace handed command_done the ID ' .. tostring(id) .. ', which is no old unit')
  end
  -- The records command-script read, in order: the gone rows of the old
  -- units come before the RESULT of seq 2 and after the RESULT of seq 1.
  local seen_result1, seen_result2, gone = false, false, {}
  for _, e in ipairs(ev) do
    if e.ev == 'result' and e.seq == 1 then
      seen_result1 = true
    elseif e.ev == 'result' and e.seq == 2 then
      seen_result2 = true
    elseif e.ev == 'gone' and old_ids[e.object] then
      assert(seen_result1 and not seen_result2, 'OBJECT_GONE of ' .. old_ids[e.object]
        .. ' is not between the RESULTs of the two spawns')
      assert(e.reason == DESTROYED, 'OBJECT_GONE reason ' .. e.reason .. ', not DESTROYED')
      gone[e.object] = (gone[e.object] or 0) + 1
    end
  end
  for id, name in pairs(old_ids) do
    assert(gone[id] == 1, name .. ' has ' .. tostring(gone[id]) .. ' OBJECT_GONE records, not 1')
  end
  S.ok('a replacing spawn writes ' .. #c2.objects .. ' OBJECT records, then one OBJECT_GONE (DESTROYED) for '
    .. 'each of the ' .. #U1 .. ' old units, then its RESULT (APPLIED)')
  local n2 = count(all, set2)
  for _, u in ipairs(U2) do
    assert(n2[u] == 1, u .. ' has ' .. tostring(n2[u]) .. ' OBJECT records at the end, not 1')
  end
  for _, u in ipairs(U1) do
    assert(count(all, set1)[u] == 1, u .. ' has more than one OBJECT record at the end')
  end
  checked_after(c2, 'the replacing spawn')
  S.ok('after ' .. hostframe .. ' frames every unit of both spawns still has exactly one OBJECT record, and '
    .. 'the births of the replacing units took no row')

  -- 4. A unit whose read raises is left out of the command and still gets one
  -- row later; the spawn is applied.
  local c3 = command(3, U3)
  expect_applied(3, 'the spawn with a raising read')
  assert(#c3.objects == #U3 - 1, #c3.objects .. ' OBJECT records before the RESULT, not ' .. (#U3 - 1))
  for _, o in ipairs(c3.objects) do
    assert(o.name ~= 'np-r-2', 'the unit whose read raised has an OBJECT before the RESULT')
  end
  local n3 = count(all, set3)
  for _, u in ipairs(U3) do
    assert(n3[u] == 1, u .. ' has ' .. tostring(n3[u]) .. ' OBJECT records at the end, not 1')
  end
  local late
  for _, l in ipairs(all) do
    if l.name == 'np-r-2' then
      late = l
    end
  end
  assert(late and late.frame > c3.done.frame, 'the raising unit\'s row did not come after the RESULT')
  S.ok('a unit whose read raises has no OBJECT before the RESULT (APPLIED), and exactly one later, in frame '
    .. late.frame .. ' after the RESULT\'s ' .. c3.done.frame)

  -- 5. Cost: the reads are inside the command.
  -- The unit whose read raised in spawn 3 gets its row at its birth, which
  -- the pump may resolve before or after this spawn.
  local c4, r4 = command(4, U4, 'np-r-2'), expect_applied(4, 'the slow spawn')
  assert(#c4.objects == #U4, #c4.objects .. ' OBJECT records in the slow spawn')
  assert(r4.cost_us >= SLOW_MS * 1000 * 0.9, 'cost_us is ' .. r4.cost_us .. ' for a read of '
    .. SLOW_MS .. ' ms: the reads are not in the command')
  S.ok('a spawn whose read takes ' .. SLOW_MS .. ' ms has cost_us ' .. r4.cost_us)
  expect_applied(5, 'the marker spawn')
  assert(over_at.before and over_at.after, 'the over-budget count was not read twice')
  assert(over_at.after - over_at.before == 1, 'commands_over_budget moved by '
    .. (over_at.after - over_at.before) .. ' across the slow spawn, not 1')
  S.ok('a spawn slower than commanded_ms = ' .. BUDGET_MS .. ' raises commands_over_budget by one')

  -- The mock queues each spawned unit's S_EVENT_BIRTH: no event reached the
  -- agent between a handler's first row and its RESULT, so the births came
  -- after the reads (UNVERIFIED in DCS), and wrote no second row (checked
  -- above, by the count of OBJECT records for each unit).
  for n, c in ipairs({ c1, c2, c3, c4 }) do
    assert(c.events == 0, c.events .. ' events reached the agent inside spawn ' .. n)
  end
  S.ok('the mock delivered the units\' S_EVENT_BIRTH after the handlers\' reads')
end

local ok, err = pcall(main)
local pid = (S.read(pidfile) or ''):match('%d+')
if pid then
  os.execute('kill ' .. pid .. ' 2>/dev/null')
end
if not ok then
  io.stderr:write(tostring(err), '\n')
  io.stderr:write('script output:\n', (S.read(out) or ''):sub(-3000), '\n')
  io.stderr:write('agent log:\n', (S.read(profile .. 'nanoprobe/logs/agent.log') or ''):sub(-2000), '\n')
end
S.remove(profile)
if not ok then
  os.exit(1)
end
