-- The rehearsal of the S15 exit test on the host: the hook and the shim in the
-- strict mock, the host-native agent, and `command-probe --part world` as the
-- sidecar, over a real mapped file and a real loopback handshake. The agent's
-- state lasts as long as the process, so tools/luatest.sh runs one scenario
-- per process: `groups`, `paused`, `names` and `debris`.
--
-- groups: 100 groups of 4 units are spawned and removed. The agent's commanded
-- work budget is 0.05 ms, so a frame's commands run over it and the probe's
-- attribution and counter checks have something to judge.
--
-- paused: the simulation is paused after a group is spawned; the group's unit
-- types change in the store, and the reconciler sends the replacing GROUP_SPAWN
-- under the group's name while paused; the simulation is resumed; the world
-- holds one group of that name with the new units, and every old unit's
-- OBJECT_GONE came. The mock repeats what a
-- paused destroy does under Wine (the removal waits for the resume).
--
-- debris: the mock's ground is 900 m up. The grid's units are killed in the
-- mock when the probe reports them spawned, and the probe clears a column
-- with no height given, then a sphere, a box and the whole map with
-- CLEAR_DEBRIS: every clear removes the wrecks inside it, the column leaves
-- every wreck outside it, the control group and the managed static stay,
-- and the 256 tiles run under the budget's attribution rule.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE, _, _, SCENARIO = ...
assert(MODULE and PROBE and (SCENARIO == 'groups' or SCENARIO == 'paused' or SCENARIO == 'names'
  or SCENARIO == 'debris'),
  'usage: lua lua/tests/world_exit.lua <module> <version> <link-probe> <inspect> <xtask> groups|paused|names|debris')
local COMMAND_PROBE = PROBE:gsub('link%-probe$', 'command-probe')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local BUDGET_MS = 0.05

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local control = profile .. 'control/'
local pidfile = profile .. 'probe.pid'

local function lines(path)
  local found = {}
  for _, line in ipairs(S.lines(path)) do
    local ok, e = pcall(S.json, line)
    if ok and type(e) == 'table' then
      found[#found + 1] = e
    end
  end
  return found
end

local function install()
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.mkdir(control)
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
  S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
  local port = 20000 + math.floor(os.clock() * 1e6 + os.time()) % 20000
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 1024\nin_ring_kib = 256\n\n"
      .. '[budget]\ncommanded_ms = %s\n', map, port, BUDGET_MS))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  return port
end

-- Run the probe on `scenario` against a fresh mock; `on_phase(host, event)` is
-- called once for each phase line the probe prints. Returns the verdict.
local function run(scenario, campaign, extra, on_phase, setup)
  local port = install()
  local out = profile .. scenario .. '.jsonl'
  local host = mock.new({ writedir = profile })
  if setup then
    setup(host)
  end
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('first', { paused = false })
  assert(os.execute(string.format(
    '%s --part world --scenario %s --campaign %d --map %s --port %d --token-file %s '
      .. '--commanded-ms %s --control-dir %s --seconds 250 --trace %s > %s 2>&1 & echo $! > %s',
    S.quote(COMMAND_PROBE), scenario, campaign, S.quote(map), port,
    S.quote(profile .. 'nanoprobe/token'), BUDGET_MS, S.quote(control), extra, S.quote(out),
    S.quote(pidfile))) == 0, 'could not start command-probe')

  local seen = {}
  local deadline = now() + 260
  local next_frame = now()
  local verdict
  while now() < deadline do
    if now() >= next_frame then
      next_frame = next_frame + 0.01
      host:frame(0.01)
      for _, e in ipairs(lines(out)) do
        if e.ev == 'phase' and not seen[e.name] then
          seen[e.name] = true
          on_phase(host, e)
        elseif e.ev == 'verdict' or e.ev == 'error' then
          verdict = e
        end
      end
      if verdict then
        break
      end
    end
  end
  local text = S.read(out) or ''
  assert(verdict, scenario .. ': no verdict within 260 s:\n' .. text:sub(-4000))
  assert(verdict.ev == 'verdict' and verdict.pass, scenario .. ': failed:\n' .. text:sub(-6000))
  return verdict
end

local function groups()
  local v = run('groups', 7, '--groups 100 --units 4', function() end)
  assert(v.spawned == 100 and v.removed == 100, 'spawned and removed')
  assert(v.units_seen == 400 and v.units_gone == 400, 'every unit came and went')
  assert(v.duplicate_spawns == 0 and v.duplicate_objects == 0 and v.duplicate_gone == 0)
  assert(v.over_budget_results_above > 0, 'the budget was too large to be exceeded')
  S.ok(string.format('%d groups of 4 spawned and removed: %d commands in %d frames, %d over budget, '
    .. '%d over-budget frames all attributed, the counter moved %d',
    v.groups, v.results, v.frames_with_commands, v.over_budget_results_above, v.over_budget_frames,
    v.counter_delta))
end

local function paused()
  local group
  local v = run('paused', 8, '--units 4 --settle 5', function(host, e)
    if e.name == 'spawned' then
      group = e.group
      host:set_pause(true)
      S.write(control .. 'paused', '')
    elseif e.name == 'replaced' then
      assert(e.code == 0, 'the replace was answered ' .. tostring(e.code) .. ' ' .. tostring(e.message))
      host:set_pause(false)
      S.write(control .. 'resumed', '')
    elseif e.name == 'settled' then
      -- The mock's world, read as a script in the server state would read it.
      local g = host.sim.groups[group]
      assert(g and g.alive, 'no group under the name')
      local n = 0
      for _, u in ipairs(g.units) do
        if u.alive then
          n = n + 1
          assert(u.type == 'BTR-80', 'an old unit is still in the group: ' .. u.name)
        end
      end
      assert(n == 4, 'the group holds ' .. n .. ' units, not 4')
      S.write(control .. 'checked', '')
    end
  end)
  assert(v.old_gone_destroyed_once == 4 and v.old_in_world == 0)
  assert(v.enumerated_under_group == 4 and v.groups_under_name == 1)
  S.ok(string.format('paused replace: one group of %d new units after the resume, %d old units gone '
    .. '(%d while paused)', v.enumerated_under_group, v.old_gone_destroyed_once, v.old_gone_while_paused))
end

-- The naming check: names in the mock read the base-62 forms of campaign
-- 1,763,000,000,000 (tag `jqW`), and a restarted sidecar rebinds them.
local function names()
  local spawned
  local v = run('names', 1763000000000, '--units 2', function(host, e)
    if e.name == 'spawned' then
      spawned = e
      assert(e.group:find('^np!jqW[0-9A-Za-z]+$'), 'group name ' .. e.group)
      assert(e.static:find('^np#jqW[0-9A-Za-z]+$'), 'static name ' .. e.static)
      assert(host.sim.groups[e.group], 'the mock has no group ' .. e.group)
      S.write(control .. 'checked', '')
    end
  end)
  assert(spawned and v.tag == 'jqW' and v.removed == 3, 'tag and removal')
  S.ok(string.format('names: %s and %s in the mock, bound, rebound after a sidecar restart with %d spawns',
    spawned.group, spawned.static, v.spawns_after_restart))
end

local function debris()
  -- The ground is 900 m up everywhere, as on a high airfield: the mock
  -- spawns the units and the static at that height.
  local GROUND = 900
  local wrecks, spawned, column_seen = {}, nil, false
  -- The live check's snippets (lua/experiments/junk/column-*.lua), run in the
  -- mock's server state as `cargo xtask commands <target> --part debris` runs
  -- them in DCS, with the same placeholders filled.
  local E = { store = {} }
  local function quoted(names)
    local out = {}
    for i, n in ipairs(names) do
      out[i] = "'" .. n .. "'"
    end
    return table.concat(out, ',')
  end
  local function snippet(host, file, fill)
    local source = assert(S.read(root .. 'lua/experiments/junk/' .. file), 'no ' .. file)
    for placeholder, text in pairs(fill) do
      source = source:gsub(placeholder, function()
        return text
      end)
    end
    assert(not source:find('__%u', 1), file .. ' has an unfilled placeholder')
    local chunk = assert(loadstring(source, '=' .. file))
    setfenv(chunk, host.states.server)
    return chunk(E)
  end
  local function read_round(host)
    return snippet(host, 'column-read.lua', {
      __INSIDE__ = quoted(spawned.column), __OUTSIDE__ = quoted(spawned.outside),
      __CONTROL__ = quoted(spawned.control), __STATIC__ = "'" .. spawned.static .. "'" })
  end
  local v = run('debris', 9, '--groups 5 --units 4', function(host, e)
    local sim = host.sim
    if e.name == 'spawned' then
      spawned = e
      local origin = snippet(host, 'column-origin.lua', { __MIN_GROUND__ = '500' })
      assert(origin == '0.0 0.0 900.0 Alpha', 'the origin on high ground is ' .. tostring(origin))
      assert(snippet(host, 'column-origin.lua', { __MIN_GROUND__ = '1000' }) == 'none 900.0',
        'an origin was found above every airbase')
      assert(snippet(host, 'column-origin.lua', { __MIN_GROUND__ = '-math.huge' }) == origin,
        'no origin without a minimum')
      -- From here on only the agent could read the ground height.
      sim.height_reads = 0
      for _, n in ipairs(e.kill) do
        local unit = assert(sim.by_name[n], 'no unit ' .. n)
        assert(unit.y == GROUND, n .. ' was spawned at ' .. unit.y .. ' m')
        wrecks[n] = unit
      end
      local hit = snippet(host, 'column-kill.lua', { __NAMES__ = quoted(e.kill) })
      assert(hit == string.format('%d %.1f', #e.kill, GROUND), 'the kill read is ' .. tostring(hit))
      for n, unit in pairs(wrecks) do
        assert(sim.dead[unit.id] == unit, 'the kill left ' .. n .. ' alive')
      end
      assert(sim.statics[e.static], 'the mock has no static ' .. tostring(e.static))
      local before = read_round(host)
      assert(before:match('^4 4 4 true:[%d.]+:900%.0 20$'), 'the read before the column is ' .. before)
      S.write(control .. 'go', string.format('%.1f', GROUND))
    elseif e.name == 'column' then
      local after = read_round(host)
      assert(after:match('^0 4 4 true:[%d.]+:900%.0 16$'), 'the read after the column is ' .. after)
      -- Only the column has cleared: its row's wrecks are gone, every other
      -- wreck is left, and the managed static and the control units stand.
      column_seen = true
      assert(e.applied and e.removed == #spawned.column,
        'the column removed ' .. tostring(e.removed) .. ', not ' .. #spawned.column)
      local inside = {}
      for _, n in ipairs(spawned.column) do
        inside[n] = true
      end
      for n, unit in pairs(wrecks) do
        if inside[n] then
          assert(sim.removed[unit.id] == unit, 'the column left the wreck of ' .. n)
        else
          assert(sim.dead[unit.id] == unit, 'the column removed the wreck of ' .. n .. ', outside it')
        end
      end
      assert(#spawned.outside == 4, 'the probe named no outside group')
      for _, n in ipairs(spawned.outside) do
        assert(not inside[n] and wrecks[n], n .. ' is not an outside wreck')
      end
      local static = assert(sim.statics[spawned.static], 'the column removed the managed static')
      assert(static.alive and static.y == GROUND, 'the managed static is not as it was spawned')
      for _, n in ipairs(spawned.control) do
        assert(sim.by_name[n] and sim.by_name[n].alive, 'the column removed the control unit ' .. n)
      end
      assert(sim.height_reads == 0, 'the agent read the ground height ' .. tostring(sim.height_reads) .. ' times')
      S.write(control .. 'column-checked', '')
    end
  end, function(host)
    host.sim.ground = GROUND
  end)
  assert(column_seen, 'the probe printed no column phase')
  local removed = {}
  for _, c in ipairs(v.clears) do
    assert(c.applied, c.clear .. ' was not applied')
    removed[c.clear] = c.removed
  end
  -- The mock's removeJunk counts the dead units inside: a group each for the
  -- column, the sphere and the box, the other two groups for the whole map.
  assert(removed.column == 4 and removed.sphere == 4 and removed.box == 4 and removed['whole map'] == 8,
    'removed ' .. tostring(removed.column) .. ', ' .. tostring(removed.sphere) .. ', ' .. tostring(removed.box)
      .. ', ' .. tostring(removed['whole map']))
  assert(v.tiles == 256, v.tiles .. ' whole-map tiles answered, not 256')
  assert(v.control_live and v.static_live and #v.control_gone == 0, 'a control unit or the managed static went')
  assert(v.tile_frames > 1 and v.over_frames > 0 and v.counter_ok,
    'the budget did not spread the tiles: ' .. v.tile_frames .. ' frames, ' .. v.over_frames .. ' over budget')
  S.ok(string.format('debris: on ground %d m up a column cleared its row with no height given and left the rest, '
    .. 'then a sphere, a box and the whole map cleared in %d frames, %d over-budget frames all attributed',
    GROUND, v.tile_frames, v.over_frames))
end

local scenarios = { groups = groups, paused = paused, names = names, debris = debris }
local ok, err = pcall(scenarios[SCENARIO])
local pid = (S.read(pidfile) or ''):match('%d+')
if pid then
  os.execute('kill ' .. pid .. ' 2>/dev/null')
end
S.remove(profile)
assert(ok, err)
