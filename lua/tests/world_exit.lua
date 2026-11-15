-- The rehearsal of the S15 exit test on the host: the hook and the shim in the
-- strict mock, the host-native agent, and `command-probe --part world` as the
-- sidecar, over a real mapped file and a real loopback handshake. The agent's
-- state lasts as long as the process, so tools/luatest.sh runs one scenario
-- per process: `groups` and `paused`.
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

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE, _, _, SCENARIO = ...
assert(MODULE and PROBE and (SCENARIO == 'groups' or SCENARIO == 'paused'),
  'usage: lua lua/tests/world_exit.lua <module> <version> <link-probe> <inspect> <xtask> groups|paused')
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
local function run(scenario, campaign, extra, on_phase)
  local port = install()
  local out = profile .. scenario .. '.jsonl'
  local host = mock.new({ writedir = profile })
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

local ok, err = pcall(SCENARIO == 'groups' and groups or paused)
local pid = (S.read(pidfile) or ''):match('%d+')
if pid then
  os.execute('kill ' .. pid .. ' 2>/dev/null')
end
S.remove(profile)
assert(ok, err)
