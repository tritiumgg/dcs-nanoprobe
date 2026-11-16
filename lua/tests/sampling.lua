-- The rehearsal of the S10 exit test (`cargo xtask sampling`) on the host:
-- the hook and the shim in the strict mock, the host-native agent,
-- `link-probe --mirror` as the sidecar, and the oracle (lua/sampling/oracle.lua)
-- read from the mock's server state, compared by `xtask sampling compare`.
--
-- `mode` is `match`, `tenth`, `deaths` or `sweep`. In `sweep` it checks the
-- host's position sweep around its edges: a getter that raises, a pause, a
-- detached link, a mission change, and frames two seconds apart, where the
-- system-call frames alternate with sampling frames (spec/transport.md,
-- "Handshake"). In `deaths` it rehearses the death
-- probe (lua/sampling/deaths.lua) with the stream recorded and the mock's
-- death delay at 30 s, the test shooting the objects a weapon kills, and
-- checks the verdict of `xtask sampling deaths` (the live run's judge): the
-- classifier's DEAD, REMOVED and MISSING, a late death event, a destroy on a
-- wreck, and a revival. In `match`, on the release build, it spawns
-- the moving, mixed scenario at 500 objects, compares the mirror with the
-- oracle, adds 1,500 more and kills some with explosions, and compares again
-- at 2,000. In `tenth`, which needs the build with fault injection, it
-- spawns 2,000 with 20 units subscribed, compares at k = 1, sets the
-- sampling budget's scale to 0.1 (spec/faults.md), goes on spawning and
-- killing, and checks that the rates fall in the stated order, the
-- enumeration period holds, the share, debt and overrun stay in bounds, and
-- the mirror still matches. The pump's p99 is the live run's to check: the
-- mock's Lua getters set it here.
--
-- The mock's getters are Lua and cost more than DCS's, so the agent runs
-- with an overhead share of 5 %, and of 2 % in `tenth`, where a tenth of the
-- budget must be short for the positions: the rehearsal checks the logic,
-- and the live run checks the budget. The full enumeration runs every 5 s
-- here, and every 20 s in `tenth`, where a shorter period would take the
-- whole tenth of the mock's budget.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE, _, XTASK, MODE = ...
MODE = MODE or 'match'
assert(MODULE and PROBE and XTASK, 'usage: lua lua/tests/sampling.lua <module> <version> <link-probe> <nanoprobe> <xtask> [match|tenth]')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local out = profile .. 'probe.jsonl'
local pidfile = profile .. 'probe.pid'
local mirror = profile .. 'mirror.json'
local oracle = profile .. 'oracle.json'

-- A small JSON encoder for the oracle's reply. Like the eval channel
-- (spec/eval.md), it takes at most 2,000 entries of a table; this one fails
-- on more, so an oracle that relies on a larger reply fails here first.
local MAX_ENTRIES = 2000
local function encode(v)
  local t = type(v)
  if t == 'table' then
    local n = 0
    for _ in pairs(v) do
      n = n + 1
    end
    assert(n <= MAX_ENTRIES, 'a reply table of ' .. n .. ' entries; the eval channel cuts it at ' .. MAX_ENTRIES)
  end
  if t == 'number' then
    return v ~= v and 'null' or string.format('%.17g', v)
  elseif t == 'string' then
    return string.format('%q', v):gsub('\\\n', '\\n')
  elseif t == 'boolean' then
    return tostring(v)
  elseif t == 'table' then
    if #v > 0 or next(v) == nil then
      local parts = {}
      for i, x in ipairs(v) do
        parts[i] = encode(x)
      end
      return '[' .. table.concat(parts, ',') .. ']'
    end
    local parts = {}
    for k, x in pairs(v) do
      parts[#parts + 1] = encode(tostring(k)) .. ':' .. encode(x)
    end
    return '{' .. table.concat(parts, ',') .. '}'
  end
  return 'null'
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
      .. "[budget]\noverhead_share = %s\n\n[sampling]\nenumeration_s = %s\n", map, port,
      MODE == 'tenth' and '0.02' or '0.05', MODE == 'tenth' and '20.0' or '5.0'))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  local A = assert(package.loadlib(version .. 'nanoprobe_agent_' .. BUILD .. '.dll', 'luaopen_nanoprobe'))()

  local host = mock.new({ writedir = profile })
  -- The hook state's position getter, counted: the hook takes it when it
  -- loads. It raises for the object ID in `getter.raise`.
  local getter = { calls = 0, raised = 0, ids = {} }
  do
    local dcs = host.states.hook.DCS
    local real = dcs.getUnitPositionByObjectId
    rawset(dcs, 'getUnitPositionByObjectId', function(id)
      getter.calls = getter.calls + 1
      getter.ids[id] = true
      if id == getter.raise then
        getter.raised = getter.raised + 1
        error('the test\'s getter raises for ' .. tostring(id), 0)
      end
      return real(id)
    end)
  end
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('sampling')
  local server = host.states.server

  local recording = profile .. 'recording.jsonl'
  local stop = profile .. 'probe.stop'
  local subscribe = (MODE == 'tenth' and ' --subscribe 20' or '') .. ' --record ' .. S.quote(recording)
    .. ' --stop-file ' .. S.quote(stop)
  local function start_probe()
    os.remove(stop)
    assert(os.execute(string.format(
      '%s --map %s --port %d --token-file %s --seconds 600 --mirror %s%s >> %s 2>&1 & echo $! > %s',
      S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(mirror), subscribe,
      S.quote(out), S.quote(pidfile))) == 0, 'could not start link-probe')
  end
  start_probe()

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

  local function attached()
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' and e.ev == 'hello' then
        return true
      end
    end
    return false
  end
  assert(run(15, attached), 'the probe did not attach:\n' .. (S.read(out) or ''))

  -- The scenario library in the server state.
  local lib = assert(loadfile(root .. 'lua/scenarios/scenario.lua'))
  setfenv(lib, server)
  local scenario = lib({ clock = os.clock, track = { defer = function() end } })

  -- Read the oracle early and run 7 s, as the live compare does. Then ask
  -- the probe for the mirror, run frames until it is written, and read it,
  -- and read the oracle in the same frame's state.
  local store = {}
  local function dump()
    local early = assert(loadfile(root .. 'lua/sampling/oracle-early.lua'))
    setfenv(early, server)
    early({ store = store })
    run(7)
    os.remove(mirror)
    S.write(mirror:gsub('%.json$', '.req'), '')
    assert(run(15, function()
      return S.read(mirror) ~= nil
    end), 'the probe wrote no mirror')
    local o = assert(loadfile(root .. 'lua/sampling/oracle.lua'))
    setfenv(o, server)
    local reply = o({ observe = function() end, store = store })
    S.write(oracle, encode(reply))
    return S.json(S.read(mirror))
  end

  -- At a tenth of the slice the rates are meant to fall: old samples are
  -- reported, not failed on.
  local function compare(label)
    local m = dump()
    -- At 500 every sample is held to its target, as in the live run.
    local cmd = string.format('%s sampling compare %s %s%s%s --recording %s', S.quote(XTASK), S.quote(oracle),
      S.quote(mirror), label == 'k = 0.1' and ' --no-ages' or '', label == '500' and ' --strict' or '',
      S.quote(recording))
    local p = io.popen(cmd)
    local verdict = p:read('*a')
    p:close()
    local v = S.json(verdict)
    assert(v.matches, label .. ': the mirror does not match the oracle: ' .. verdict)
    return v, m
  end

  -- How many lines of the probe's output hold `text`.
  local function lines_with(text)
    local n = 0
    for _, line in ipairs(S.lines(out)) do
      if line:find(text, 1, true) then
        n = n + 1
      end
    end
    return n
  end

  if MODE == 'sweep' then
    local spawned = scenario.spawn_mixed(60, { label = 'a' })
    run(8)
    assert(getter.calls > 60, 'the frame swept no positions: ' .. getter.calls .. ' reads')
    compare('60')
    S.ok('the frame sweeps the positions in the hook state, and the mirror matches')

    -- A getter that raises: the row is left out, the sweep and the frames
    -- go on, and the Lua state is as it was (a stack left unbalanced by the
    -- entry would raise in the hook within a few frames).
    local victim = server.Unit.getByName('np-a-u1-1').id_
    getter.raise = victim
    local before = getter.calls
    run(3)
    assert(getter.raised >= 1, 'the getter was not asked for the unit')
    assert(getter.raised <= 2, 'the row was read again after it raised: ' .. getter.raised .. ' times')
    assert(getter.calls - before > 60, 'the sweep stopped after the getter raised')
    getter.raise = nil
    -- The next enumeration finds the unit and its row is read again.
    run(8)
    local reads = getter.raised
    getter.ids = {}
    run(6)
    assert(getter.ids[victim], 'the row was not read again after an enumeration found the unit')
    assert(getter.raised == reads, 'the getter raised again')
    compare('after a raise')
    S.ok('a getter that raises costs its row one enumeration and nothing else')

    -- Paused: sampling goes on (spec/sampling.md, "The frame").
    host:set_pause(true)
    run(1)
    before = getter.calls
    run(3)
    assert(getter.calls - before > 60, 'no sweep while paused: ' .. (getter.calls - before) .. ' reads')
    host:set_pause(false)
    S.ok('the sweep goes on while the simulation is paused')

    -- Detached: no sweep. Attached again: the sweep and the mirror return.
    local hellos = lines_with('"ev":"hello"')
    S.write(stop, '')
    assert(run(15, function()
      return lines_with('"ev":"end"') > 0
    end), 'the probe did not stop')
    run(2)
    before = getter.calls
    run(3)
    assert(getter.calls == before, 'the frame swept while detached: ' .. (getter.calls - before) .. ' reads')
    start_probe()
    assert(run(15, function()
      return lines_with('"ev":"hello"') > hellos
    end), 'the probe did not attach again')
    run(10)
    compare('attached again')
    S.ok('no sweep while detached, and the mirror matches after the next attach')

    -- A mission change: the rows of the old mission are never read again.
    host:load_mission('second')
    getter.ids = {}
    local old = {}
    for _, name in ipairs(spawned.groups) do
      old[#old + 1] = name
    end
    run(3)
    assert(next(getter.ids) == nil, 'the frame read a position with no object in the new mission')
    scenario.clear()
    scenario.spawn_mixed(60, { label = 'b' })
    run(10)
    assert(next(getter.ids) ~= nil, 'no sweep in the new mission')
    compare('second mission')
    S.ok('a mission change empties the sweep, and it starts again with the new objects')

    -- Frames 2 s apart: every frame is due its system calls, and with a
    -- sidecar attached they come on every second frame only, so the frames
    -- between sample. The reload request and the kill switch are seen
    -- within two frames.
    local function slow_frame()
      local t0 = now()
      while now() - t0 < 2 do
      end
      local reads = getter.calls
      host:frame(2)
      return getter.calls > reads
    end
    local swept = {}
    for i = 1, 6 do
      swept[i] = slow_frame()
    end
    local alternates = true
    for i = 2, #swept do
      alternates = alternates and swept[i] ~= swept[i - 1]
    end
    assert(alternates, 'at 0.5 frames a second the sweeps did not alternate: ' .. encode(swept))
    S.write(profile .. 'nanoprobe/reload', '')
    local reloads = lines_with('shim reloaded')
    slow_frame()
    slow_frame()
    run(0.2)
    -- The hook's record of the reload reaches the sidecar a frame later.
    assert(S.read(profile .. 'nanoprobe/reload') == nil, 'the reload request was not taken within two frames')
    assert(lines_with('"ev":"end"') == 1, 'the probe lost the link at 0.5 frames a second')
    S.write(profile .. 'nanoprobe/disable', '')
    slow_frame()
    slow_frame()
    assert((S.read(profile .. 'nanoprobe/logs/hook.log') or ''):find('kill switch found', 1, true),
      'the kill switch was not seen within two frames')
    local _ = reloads
    S.ok('at 0.5 frames a second sampling frames alternate with system-call frames')
  elseif MODE == 'deaths' then
    -- The mock models no gunfire: the test shoots the objects of the stages
    -- that a weapon kills, at fixed times, and the script does the rest.
    -- The death events come 30 s of model time after a kill (LIVE: 0 to
    -- 84 s). Frames run at 0.02 s of model time, so a second of wall time
    -- is two of model time and the 5 s enumeration is 10.
    local sim = host.sim
    sim.death_delay = 30
    local DT = 0.02
    local function run_model(seconds, done)
      local deadline = now() + seconds
      local next_frame = now()
      while now() < deadline do
        if now() >= next_frame then
          next_frame = next_frame + 0.01
          host:frame(DT)
          if done and done() then
            return true
          end
        end
      end
      return done == nil
    end
    local function verdict(file, probe_json, extra)
      S.write(file, encode(probe_json))
      local p = io.popen(string.format('%s sampling deaths %s %s %s 2>&1', S.quote(XTASK), S.quote(file),
        S.quote(recording), extra or ''))
      local text = p:read('*a')
      p:close()
      return S.json(text), text
    end

    -- Part A: the live script, on the mock.
    local source = S.read(root .. 'lua/sampling/deaths.lua'):gsub('@WATCH@', '40'):gsub('@GIVE_UP@', '40')
      :gsub('@WRECK@', '22')
    local probe = assert(loadstring(source, '=deaths.lua'))
    setfenv(probe, server)
    local t0 = host.model
    local waiting = probe({
      lib = function()
        return scenario
      end,
      wait = function(fn)
        return { fn = fn }
      end,
      clock = os.clock,
    })
    local shots = {
      { 6.0, { 'np-deaths-b1', 'np-deaths-b2', 'np-deaths-b3', 'np-deaths-r1', 'np-deaths-r2' } },
      { 6.5, { 'np-deaths-t1', 'np-deaths-t2', 'np-deaths-t3', 'np-deaths-t4', 'np-deaths-v1', 'np-deaths-v2' } },
      { 7.0, { 'np-deaths-sw1', 'np-deaths-sw2', 'np-deaths-sw3', 'np-deaths-sw4' } },
      { 8.0, { 'np-deaths-victim1-1', 'np-deaths-victim2-1', 'np-deaths-victim3-1' } },
    }
    local found
    assert(run_model(120, function()
      for _, shot in ipairs(shots) do
        if shot[1] and host.model - t0 >= shot[1] then
          local shooter = sim.by_name['np-deaths-cas-1']
          for _, name in ipairs(shot[2]) do
            local target = assert(sim.by_name[name] or sim.statics[name], 'nothing named ' .. name)
            sim:shoot(target, shooter)
          end
          shot[1] = nil
        end
      end
      found = waiting.fn()
      return found ~= nil
    end), 'the death probe did not end')
    for key, ok in pairs(found.actions) do
      assert(ok, 'the probe did not act: ' .. key .. ' ' .. encode(found.actions))
    end
    for name, taken in pairs(found.spawned) do
      assert(taken == true, name .. ' was not added: ' .. tostring(taken))
    end
    -- The last records reach the recording.
    run_model(1)
    local v, text = verdict(profile .. 'deaths.json', found, '--catch-s 30')
    local function unit(name)
      return assert(v.units[name], 'the verdict lacks ' .. name .. ': ' .. text)
    end
    assert(v.ok and v.complete, 'the verdict: ' .. text)
    assert(#v.failures == 0 and v.statics_ok, 'failures: ' .. text)
    assert(v.kills.weapon == 11 and v.kills.explosion == 4 and v.kills.unit == 5 and v.kills.group == 5
      and v.kills.static_weapon == 4 and v.kills.static_remove == 3 and v.kills.wreck == 3 and v.kills.shootdown == 3,
      'the kills: ' .. encode(v.kills))
    -- A weapon kill is DEAD within a pump of its HIT. The live judge allows
    -- 0.5 s; the debug build here pumps a few times a second of wall time.
    for _, name in ipairs({ 'np-deaths-b1', 'np-deaths-t3', 'np-deaths-sw2' }) do
      local u = unit(name)
      assert(u.triggered and #u.records == 1 and u.records[1].reason == 'DEAD' and u.records[1].after_gone_s <= 4,
        name .. ': ' .. encode(u))
    end
    -- An explosion has no HIT: the check an enumeration queues finds it.
    for _, name in ipairs({ 'np-deaths-e1', 'np-deaths-se2', 'np-deaths-w3' }) do
      local u = unit(name)
      assert(not u.triggered and #u.records == 1 and u.records[1].reason == 'DEAD', name .. ': ' .. encode(u))
    end
    -- A removal is REMOVED and stays so; a destroy on a wreck leaves its
    -- DEAD standing.
    for _, name in ipairs({ 'np-deaths-u2', 'np-deaths-gd3-1', 'np-deaths-sr1' }) do
      local u = unit(name)
      assert(#u.records == 1 and u.records[1].reason == 'REMOVED', name .. ': ' .. encode(u))
    end
    assert(unit('np-deaths-w1').read_at_end == 'removed', 'the wreck was not destroyed: ' .. encode(unit('np-deaths-w1')))
    -- An aircraft leaves MISSING, and its late death events make it DEAD.
    for _, name in ipairs({ 'np-deaths-victim1-1', 'np-deaths-victim3-1' }) do
      local u = unit(name)
      assert(#u.records == 2 and u.records[1].reason == 'MISSING' and u.records[2].reason == 'DEAD'
        and u.records[2].after_gone_s >= 29, name .. ': ' .. encode(u))
    end
    assert(#v.late_dead_after_missing == 3, 'late DEADs: ' .. text)
    -- No DEAD by the read lacks its death event.
    assert(#v.unconfirmed_dead == 0, 'unconfirmed: ' .. text)
    assert(v.dead_by_read >= 7, 'DEADs by the read: ' .. text)
    local survivor = unit('np-deaths-u6')
    assert(not survivor.ceased and #survivor.records == 0, 'the control: ' .. encode(survivor))
    assert(found.isexist_calls > 0 and found.isexist_us and found.isexist_us < 1000, 'the check cost: ' .. encode(found.isexist_us))
    S.ok('the death probe on the mock: kills are DEAD, removals REMOVED, aircraft MISSING then DEAD, and the verdict passes')

    -- Part B: the classifier's edges, on units of the test's own.
    local usa = server.country.id.USA
    local ids = {}
    local function add(name, x)
      sim:add_group(usa, 2, { name = name, units = { { name = name .. '-1', type = 'M-113', x = x, y = 0 } } })
      return sim.by_name[name .. '-1']
    end
    local rev, survivor2, named = add('np-rv', 30000), add('np-sv', 30100), add('np-nm', 30200)
    sim:add_static(usa, { name = 'np-rs', type = 'Fuel tank', x = 30300, y = 0 })
    run_model(2)
    local shooter = sim.by_name['np-deaths-cas-1'] or rev
    -- A HIT on a survivor, and a death event on a live row, change nothing.
    sim:hit(survivor2, shooter)
    sim.queue[#sim.queue + 1] = { event = 'S_EVENT_DEAD', initiator = named }
    sim.queue[#sim.queue + 1] = { event = 'S_EVENT_UNIT_LOST', initiator = named }
    -- A kill and, in the same second, a destroy of the wreck; a killed
    -- static is destroyed later too.
    sim:shoot(rev, shooter)
    local depot = sim.statics['np-rs']
    sim:shoot(depot, shooter)
    run_model(2)
    local before = host.model
    -- Revive the unit at once, before its death events: same object ID.
    host.states.mission.a_unit_set_life_percentage(rev.mission_id, 100)
    assert(server.Object.isExist({ id_ = rev.id }), 'the unit did not revive')
    run_model(2)
    server.Object.destroy({ id_ = depot.id })
    -- The enumeration finds the revived unit and the death events, 30 s
    -- after the kill, change nothing.
    run_model(40)
    assert(server.Object.isExist({ id_ = rev.id }), 'a late death event removed the revived unit')
    -- Killed again: a second DEAD.
    sim:shoot(rev, shooter)
    run_model(6)
    run_model(1)
    for name, thing in pairs({ ['np-rv-1'] = rev, ['np-sv-1'] = survivor2, ['np-nm-1'] = named, ['np-rs'] = depot }) do
      ids[name] = { id = thing.id, stage = 'edge' }
    end
    local w, wtext = verdict(profile .. 'edges.json', { units = ids, ended = host.model, seconds = 0 })
    local function edge(name)
      return assert(w.units[name], 'the verdict lacks ' .. name .. ': ' .. wtext)
    end
    local reasons = {}
    for _, r in ipairs(edge('np-rv-1').records) do
      reasons[#reasons + 1] = r.reason
    end
    assert(table.concat(reasons, ',') == 'DEAD,DEAD', 'the revived unit\'s records: ' .. table.concat(reasons, ','))
    -- The revival is an OBJECT between the two deaths, with the same ID.
    local first, second = edge('np-rv-1').records[1].t, edge('np-rv-1').records[2].t
    local between = 0
    for _, t in ipairs(edge('np-rv-1').objects) do
      if t > first and t < second then
        between = between + 1
      end
    end
    assert(between >= 1, 'no OBJECT for the revived unit: ' .. encode(edge('np-rv-1')))
    assert(edge('np-sv-1').records[1] == nil and edge('np-nm-1').records[1] == nil,
      'a HIT or a death event on a live row removed it: ' .. wtext)
    assert(#edge('np-rs').records == 1 and edge('np-rs').records[1].reason == 'DEAD',
      'a killed static that was destroyed: ' .. encode(edge('np-rs')))
    assert(host.model > before)
    S.ok('a HIT or a death event on a live row changes nothing, a destroyed wreck keeps its DEAD, a revived unit comes back and dies twice')

    -- Part B2: the death events the shim registers, S_EVENT_CRASH and
    -- S_EVENT_UNIT_LOST, mark an aircraft that is still in the table; the
    -- next pump after it leaves reads DEAD, where an aircraft with no event
    -- reads MISSING.
    local fliers = {}
    for _, kind in ipairs({ 'crash', 'lost', 'none' }) do
      local name = 'np-air-' .. kind
      sim:add_group(usa, 0, { name = name, units = { { name = name .. '-1', type = 'F-15C', x = 32000, y = 0 } } })
      fliers[kind] = sim.by_name[name .. '-1']
    end
    run_model(3)
    -- A crashed aircraft's event arrives with the unit already gone from
    -- the world and its row still in the agent's table: the event is the
    -- frame's, and the removal is the same frame's, so no enumeration comes
    -- between them.
    for _, f in pairs(fliers) do
      assert(server.Object.isExist({ id_ = f.id }), f.name .. ' was not in the world')
      sim:remove_unit(f)
    end
    sim.queue[#sim.queue + 1] = { event = 'S_EVENT_CRASH', initiator = fliers.crash }
    sim.queue[#sim.queue + 1] = { event = 'S_EVENT_UNIT_LOST', initiator = fliers.lost }
    host:frame(DT)
    run_model(4)
    local air_ids = {}
    for kind, f in pairs(fliers) do
      air_ids[f.name] = { id = f.id, stage = 'edge' }
    end
    local a, atext = verdict(profile .. 'air.json', { units = air_ids, ended = host.model, seconds = 0 })
    for kind, expect in pairs({ crash = 'DEAD', lost = 'DEAD', none = 'MISSING' }) do
      local u = assert(a.units[fliers[kind].name], 'the verdict lacks ' .. kind .. ': ' .. atext)
      assert(#u.records == 1 and u.records[1].reason == expect,
        kind .. ' aircraft records: ' .. encode(u.records) .. ' wanted ' .. expect)
    end
    S.ok('S_EVENT_CRASH and S_EVENT_UNIT_LOST on an aircraft still in the table make it DEAD when it leaves; with no event it is MISSING')

    -- Part C: a coalition's static list runs in an enumeration batch of its
    -- own. The mock's getStaticObjects takes 2 ms; the other calls of an
    -- enumeration (the group list, a group's units, a static's point and
    -- description) are noted by the batch they ran in: a batch starts at each
    -- call of the agent's `plan`, which the shim's pump makes once a batch.
    for i = 1, 4 do
      sim:add_static(usa, { name = 'np-st' .. i, type = 'Fuel tank', x = 31000 + i * 50, y = 0 })
    end
    local by_frame, slow_calls, slow_s = { {} }, 0, 0
    local function note(kind)
      local f = by_frame[#by_frame]
      f[kind] = (f[kind] or 0) + 1
    end
    local shim = server.__nanoprobe_shim
    local agent
    for i = 1, 20 do
      local name, value = debug.getupvalue(shim.pump, i)
      if name == 'agent' then
        agent = value
      end
    end
    assert(type(agent) == 'table', 'the shim\'s agent table was not found')
    local plan = agent.plan
    rawset(agent, 'plan', function(...)
      by_frame[#by_frame + 1] = {}
      return plan(...)
    end)
    local function wrap(tbl, name, kind, spin)
      local real = tbl[name]
      rawset(tbl, name, function(...)
        note(kind)
        if spin then
          local t = os.clock()
          while os.clock() - t < 0.002 do
          end
          slow_calls, slow_s = slow_calls + 1, os.clock() - t
        end
        return real(...)
      end)
    end
    wrap(server.coalition, 'getStaticObjects', 'statics', true)
    wrap(server.coalition, 'getGroups', 'other')
    wrap(server.Group, 'getUnits', 'other')
    wrap(server.StaticObject, 'getDesc', 'other')
    run_model(14)
    assert(slow_calls >= 2, 'the static list was called ' .. slow_calls .. ' times')
    local alone = 0
    for model, f in pairs(by_frame) do
      if f.statics then
        alone = alone + 1
        assert(not f.other, 'a batch ' .. model .. ' ran the static list and ' .. encode(f))
      end
    end
    assert(alone >= 2, 'too few frames ran the static list: ' .. alone)
    local longest = 0
    for _, line in ipairs(S.lines(profile .. 'nanoprobe/logs/agent.log')) do
      local ok, e = pcall(S.json, line)
      local d = ok and type(e) == 'table' and e.msg == 'sampling' and e.data
      if d and d.enum_step_max_us and d.enum_step_max_us > longest then
        longest = d.enum_step_max_us
      end
    end
    assert(longest >= slow_s * 1e6 * 0.9, 'enum_step_max_us ' .. longest .. ' is below the static list\'s ' .. slow_s * 1e6)
    S.ok('the static list runs in a batch of its own, and enum_step_max_us reports its time')
  elseif MODE == 'match' then
    -- Births go before a running enumeration (spec/sampling.md, "Tiers"), so
    -- a spawn fills the table at a birth's cost and may outlast a fixed wait.
    -- Wait for a `sampling` report that holds `n` rows and no birth waiting,
    -- at most `limit` seconds, then run one more lap of every tier.
    local function filled(n, limit)
      -- The log is read twice a second, not on every frame.
      local next_read = now()
      local function done()
        if now() < next_read then
          return false
        end
        next_read = now() + 0.5
        local last
        for _, line in ipairs(S.lines(profile .. 'nanoprobe/logs/agent.log')) do
          local ok, e = pcall(S.json, line)
          local d = ok and type(e) == 'table' and e.msg == 'sampling' and e.data
          if d then
            last = d
          end
        end
        return last ~= nil and last.births == 0 and last.rows >= n
      end
      assert(run(limit, done), 'the spawn of ' .. n .. ' did not resolve in ' .. limit .. ' s')
    end
    local first = scenario.spawn_mixed(500, { label = 'a' })
    assert(first.ships > 0 and first.air > 0 and first.statics > 0, 'the mix lacks a kind: ' .. encode(first))
    -- Births resolve, the first enumeration ends, every row gets a position
    -- and vitals: the vitals period is 5 s.
    filled(500, 60)
    run(8)
    local v = compare('500')
    assert(v.oracle == 500, 'the oracle read ' .. v.oracle .. ' objects')
    S.ok('at 500 moving, mixed objects the mirror matches the oracle')

    local second = scenario.spawn_mixed(1500, { label = 'b' })
    run(3)
    local hit = scenario.churn(second.groups, 20)
    assert(#hit == 20, 'churn hit ' .. #hit .. ' units')
    filled(1960, 120)
    run(8)
    local v2, m = compare('2000')
    assert(v2.oracle >= 1960 and v2.oracle < 2000, 'the oracle read ' .. v2.oracle .. ' objects')
    assert(m.counters.disagreements == 0, 'the mirror and the agent disagree on an enumeration')
    S.ok('at 2,000 moving, mixed objects, with deaths, the mirror matches the oracle')

    -- The periods are reported, near their targets.
    local p = m.periods
    -- The vitals period is not judged here: a pass that partial batches end
    -- early reports under 4 s in some runs of the mock.
    assert(p[1] > 0.8 and p[1] < 1.5, 'position period ' .. p[1])
    assert(p[2] > 0, 'no vitals period reported')
    S.ok('the status block reports the achieved periods near their targets')
  else
    -- The same run at k = 1 and at k = 0.1 (spec/faults.md, `sampling`).
    local spawned = scenario.spawn_mixed(2000, { label = 'a' })
    -- Past the second enumeration, which finds the statics.
    run(24)
    local _, m1 = compare('k = 1')
    assert(#m1.subscribed == 20, 'subscribed ' .. #m1.subscribed)
    assert(A.inject('sampling', 0.1), 'the module has no fault injection')
    local mark = #S.lines(out)
    -- Births go on at k = 0.1: a new group, and deaths.
    run(15)
    scenario.spawn_mixed(50, { label = 'c' })
    scenario.churn(spawned.groups, 10)
    run(15)
    -- Compare once the mirror holds every object DCS holds (S10 ruling 3c).
    -- New objects enter through the enumeration, which runs back to back at
    -- k = 0.1: within one period plus one enumeration's duration, about
    -- 100 s each here.
    local t0 = now()
    local filled
    while not filled and now() - t0 < 240 do
      run(6)
      local m = dump()
      local o = S.json(S.read(oracle))
      local n = 0
      for _, c in ipairs(o.chunks or {}) do
        n = n + #c
      end
      filled = #m.rows == n
    end
    assert(filled, 'k = 0.1: the mirror did not fill within 240 s')
    local _, m2 = compare('k = 0.1')

    -- The subscribed rows' own sample intervals, from the mirror.
    local sub = {}
    for _, id in ipairs(m2.subscribed) do
      sub[id] = true
    end
    local subscribed = 0
    for _, row in ipairs(m2.rows) do
      if sub[row.object] and row.interval then
        subscribed = math.max(subscribed, row.interval)
      end
    end
    -- The order is judged over the span of the reports: a tier's reported
    -- period includes any stall in progress, so one instant is noise.
    local function reports_of(scale, from, to)
      local found = {}
      for i, line in ipairs(S.lines(out)) do
        local ok, e = pcall(S.json, line)
        if i > from and i <= to and ok and type(e) == 'table' and e.ev == 'log' and e.msg == 'sampling'
          and type(e.data) == 'table' and e.data.scale == scale then
          found[#found + 1] = e.data
        end
      end
      return found
    end
    -- The last two reports at k = 1 before the change; the reports at
    -- k = 0.1 less the first, whose window straddles it.
    local k1, k01 = reports_of(1, 0, mark), reports_of(0.1, mark, math.huge)
    local function tail(set, from)
      local rest = {}
      for i = math.max(1, from), #set do
        rest[#rest + 1] = set[i]
      end
      return rest
    end
    k1, k01 = tail(k1, #k1 - 1), tail(k01, 2)
    assert(#k1 >= 1 and #k01 >= 1, #k1 .. ' reports at k = 1, ' .. #k01 .. ' at k = 0.1')
    local function rate(set, key)
      local rows, window = 0, 0
      for _, d in ipairs(set) do
        rows, window = rows + d[key], window + d.window_s
      end
      return window > 0 and rows / window or 0
    end
    -- The share of its rows a tier kept; nil when it owed none at k = 1.
    local function kept(key)
      local base = rate(k1, key)
      return base > 0 and rate(k01, key) / base or nil
    end
    local function mean(key)
      local sum = 0
      for _, d in ipairs(k01) do
        sum = sum + d[key]
      end
      return sum / #k01
    end
    local kpos, ksub, kvit = kept('rows_positions'), kept('rows_subscribed'), kept('rows_vitals')
    local kwh = kept('rows_warehouses')
    local pos, vit, wh = mean('position_s'), mean('vitals_s'), mean('warehouse_s')
    local enum = m2.periods[4]
    local function show(v)
      return v and string.format('%.2f', v) or 'not judged'
    end
    local kept_text = string.format('kept: subscribed %s, positions %s, vitals %s, '
      .. 'warehouses %s', show(ksub), show(kpos), show(kvit), show(kwh))
    -- Births and the enumeration rank above subscribed positions, so these
    -- slow while they run; they must keep a rate.
    assert(subscribed > 0 and subscribed < 5, 'subscribed rows sampled ' .. subscribed .. ' s apart')
    -- The lower tiers stretch first: each keeps no more of its rate than the one above.
    local function le(a, b)
      return a == nil or b == nil or a <= b + 0.02
    end
    -- Warehouses are left out: they read a few rows a window, so their kept
    -- share swings by whole rows (spec/faults.md: k = 0.1 is a test scale).
    assert(le(kvit, kpos) and le(kpos, ksub),
      'the lower tiers did not stretch first; ' .. kept_text)
    -- Subscribed positions fall last: they keep their rate at a tenth.
    assert(ksub == nil or ksub >= 0.9, 'subscribed rows kept ' .. show(ksub) .. ' of their rate')
    -- A tenth must be short for the positions here, or the order is not
    -- tested (the share in `tenth` mode is set for that).
    assert(kpos ~= nil and kpos < 0.85, 'positions kept ' .. show(kpos) .. ' of their rate at k = 0.1')
    -- At k = 0.1 subscribed positions keep a rate before the enumeration
    -- keeps its period (S10 ruling 3b): it must still complete.
    assert(enum > 0, 'no enumeration completed at k = 0.1')
    S.ok(string.format('at k = 0.1 the rates fall in order: %s; mean periods: subscribed %.2f s, '
      .. 'positions %.2f s, vitals %.2f s, warehouses %.2f s; enumeration %.2f s',
      kept_text, subscribed, pos, vit, wh, enum))

    -- The overhead holds: the sampling reports after the change.
    local reports, rows, over, max_debt, share_sum = 0, 0, 0, 0, 0
    local max_over, empty, pumps, lone, checkp, over_us = 0, 0, 0, 0, 0, 0
    for i, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if i > mark and ok and type(e) == 'table' and e.ev == 'log' and e.msg == 'sampling'
        and type(e.data) == 'table' and e.data.scale == 0.1 then
        reports = reports + 1
        local d = e.data
        assert(d.share <= 0.02, 'share ' .. d.share)
        -- The mock's getters time unevenly under load, so the bound stays loose.
        assert(d.debt_us <= 0.02 * 1e6, 'debt ' .. d.debt_us)
        max_debt = math.max(max_debt, d.debt_us)
        share_sum = share_sum + d.share
        -- Past the budget by more than one clock interval only in rare
        -- frames, such as a garbage collection step inside a getter.
        rows, over = rows + d.row_frames, over + d.overrun_frames
        max_over = math.max(max_over, d.overrun_us or 0)
        empty, pumps, lone = empty + (d.empty_pumps or 0), pumps + (d.pumps or 0), lone + (d.lone_rows or 0)
        checkp = checkp + (d.check_pumps or 0)
        over_us = over_us + (d.overrun_sum_us or 0)
      end
    end
    assert(reports >= 2, 'only ' .. reports .. ' sampling reports at k = 0.1')
    -- At k = 0.1 the gate asks for a pump that can pay for a row once the
    -- agent is in debt, so few pumps plan nothing: 8 to 10 % of pumps over
    -- five runs on the mock debug build, against 41 to 45 % without it. The
    -- bound leaves room for a loaded machine.
    assert(empty * 5 <= pumps, string.format('%d of %d pumps at k = 0.1 planned no row', empty, pumps))
    -- Row frames past their budget by more than a clock interval are
    -- reported, not failed: 41 to 69 % over five runs with the gate and three
    -- without, mostly one-batch pumps for the subscribed rows that run 4 to
    -- 14 us past a budget of about 18 us (the batch's close and the pump's
    -- own steps on the unoptimized mock). The gate does not change the count.
    -- The live run judges it on the release build in DCS (crates/xtask,
    -- `tenth`). Forgiven debt and the largest overrun are reported too.
    S.ok(string.format('at k = 0.1 the share and debt stay within their bounds, and the mirror '
      .. 'matches (overrun frames %d of %d, %.0f us in all, largest %.0f us; empty pumps %d of %d pumps, '
      .. '%d with checks; lone rows %d)', over, rows, over_us, max_over, empty, pumps, checkp, lone))
  end
end

local ok, err = pcall(main)
local pid = S.read(pidfile)
if pid then
  os.execute('kill ' .. pid:match('%d+') .. ' 2>/dev/null')
end
if not ok then
  io.stderr:write(tostring(err), '\n')
  io.stderr:write('probe output:\n', (S.read(out) or ''):sub(-3000), '\n')
  io.stderr:write('shim log:\n', (S.read(profile .. 'nanoprobe/logs/shim.log') or ''):sub(-2000), '\n')
  io.stderr:write('agent log:\n', (S.read(profile .. 'nanoprobe/logs/agent.log') or ''):sub(-2000), '\n')
  os.exit(1)
end
