-- The rehearsal of the S17 live test on the host: the sidecar program
-- (`nanoprobe-sidecar run`, over RealSim) attached to the host-native agent in
-- the strict mock, and both hello campaigns run against it, one after the
-- other, unchanged. Model time runs five times faster than real time, so a
-- hello's route ends well inside its own time limit.
--
-- usage: lua lua/tests/sidecar.lua <module> <version> <link-probe> ...
-- The Rust hello and the sidecar are beside <link-probe>. The Python hello
-- runs when `uv` is on the PATH and its environment exists.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE = ...
assert(MODULE and PROBE, 'usage: lua lua/tests/sidecar.lua <module> <version> <link-probe> ...')
local bin = PROBE:match('^(.*/)')
local SIDECAR = bin .. 'nanoprobe-sidecar'
local HELLO_RUST = bin .. 'hello-campaign'

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
-- Each hello's limit, in CPU seconds of this loop; the hellos have their own.
local LIMIT_S = 150
local MODEL_STEP = 0.05

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'

S.mkdir(profile .. 'Scripts/Hooks')
S.mkdir(version .. 'lua')
S.mkdir(profile .. 'nanoprobe/logs')
S.mkdir(profile .. 'sidecar')
S.mkdir(profile .. 'nanoprobe/missions')
-- `[missions]`: activation loads `<theater>.miz` from this folder.
S.write(profile .. 'nanoprobe/missions/Caucasus.miz', 'shell')
S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
local port = 20000 + math.floor(os.clock() * 1e6 + os.time()) % 20000
S.write(profile .. 'nanoprobe/agent.toml', string.format(
  "[transport]\nmap_path = '%s'\nport = %d\n", map, port))
S.write(profile .. 'nanoprobe/token', TOKEN)
S.write(profile .. 'sidecar/sidecar.toml', string.format(
  "[agent]\nmap_path = '%s'\nhandshake = '127.0.0.1:%d'\ntoken_path = '%s'\n\n"
    .. "[api]\nlisten = '127.0.0.1:%d'\n\n[store]\ndir = 'campaigns'\n\n"
    .. "[missions]\ndir = '%s'\ndcs_dir = '%s'\n",
  map, port, profile .. 'nanoprobe/token', port + 1,
  profile .. 'nanoprobe/missions', profile .. 'nanoprobe/missions'))
local endpoint = string.format('http://127.0.0.1:%d', port + 1)

local host = mock.new({ writedir = profile })
host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
host:load_mission('first', { paused = false })

-- Start `command` in the background; its output goes to `name`.out and its
-- exit code to `name`.code. Returns the command's pid.
local function start(name, command)
  local base = profile .. name
  assert(os.execute(string.format('(%s & echo $! > %s.tmp; mv %s.tmp %s; wait $!; echo $? > %s) > %s 2>&1 &',
    command, S.quote(base .. '.pid'), S.quote(base .. '.pid'), S.quote(base .. '.pid'),
    S.quote(base .. '.code'), S.quote(base .. '.out'))) == 0,
    'could not start ' .. name)
  local deadline = now() + 5
  while now() < deadline do
    local pid = (S.read(base .. '.pid') or ''):match('%d+')
    if pid then
      return pid
    end
  end
  error('no pid for ' .. name)
end

-- The mock's world has no AI: it keeps orders and obeys none. A route order
-- (a `Mission` task) is obeyed here, the way DCS would, so the hello's group
-- drives to its route's end.
local obeyed, seen_orders = 0, nil
local function obey_routes()
  local orders = host.sim.orders
  -- A mission load makes a new world with a new list of orders.
  if orders ~= seen_orders then
    obeyed, seen_orders = 0, orders
  end
  while obeyed < #orders do
    obeyed = obeyed + 1
    local o = orders[obeyed]
    local task = o.task
    local points = type(task) == 'table' and task.id == 'Mission' and task.params
      and task.params.route and task.params.route.points
    if points then
      for _, g in pairs(host.sim.groups) do
        if g.id == o.group or tostring(g.id) == tostring(o.group) then
          g.route = {}
          local lead = g.units[1]
          if lead then
            g.route[1] = { x = lead.x, z = lead.z, speed = 0 }
          end
          for _, p in ipairs(points) do
            g.route[#g.route + 1] = { x = p.x, z = p.y, speed = p.speed or 0 }
          end
          g.leg = 1
        end
      end
    end
  end
end

local function frames_until(done, limit, what)
  local deadline = now() + limit
  local next_frame = now()
  while now() < deadline do
    if now() >= next_frame then
      next_frame = next_frame + 0.01
      host:frame(MODEL_STEP)
      obey_routes()
      if done() then
        return
      end
    end
  end
  error(what .. ': not done within ' .. limit .. ' s')
end

local sidecar_pid = start('sidecar', string.format('%s run --config %s',
  S.quote(SIDECAR), S.quote(profile .. 'sidecar/sidecar.toml')))
local ok, err = pcall(function()
  frames_until(function()
    return (S.read(profile .. 'sidecar.out') or ''):find('listening on', 1, true)
  end, 20, 'the sidecar listening')

  local hellos = { { 'rust', S.quote(HELLO_RUST) .. ' ' .. endpoint } }
  -- The Python hello runs once uv has made its environment (`mise run
  -- hello-python-gen`), so the test needs no network.
  if os.execute('command -v uv > /dev/null 2>&1') == 0
    and os.execute('test -d ' .. S.quote(root .. 'examples/hello-python/.venv')) == 0 then
    assert(os.execute('sh ' .. S.quote(root .. 'examples/hello-python/generate.sh') .. ' > /dev/null') == 0,
      'could not generate the Python hello')
    hellos[#hellos + 1] = { 'python', string.format(
      'uv run --frozen --project %s python %s %s',
      S.quote(root .. 'examples/hello-python'), S.quote(root .. 'examples/hello-python/hello.py'),
      endpoint) }
  else
    print('skip  the Python hello: no uv, or no examples/hello-python/.venv (mise run hello-python-gen)')
  end
  local mission = profile .. 'nanoprobe/missions/Caucasus.miz'
  for _, h in ipairs(hellos) do
    local name = 'hello-' .. h[1]
    local loads = #host.loads
    -- Nothing of the hello's campaign spawns into the mission that runs
    -- before its activation's load.
    local function groups()
      local n = 0
      for _ in pairs(host.sim.groups) do
        n = n + 1
      end
      return n
    end
    local before, early = groups(), 0
    start(name, h[2])
    frames_until(function()
      if #host.loads == loads then
        early = math.max(early, groups() - before)
      end
      return S.read(profile .. name .. '.code')
    end, LIMIT_S, name)
    assert(early == 0, name .. ': ' .. early .. ' group(s) spawned into the mission before the load')
    local code = (S.read(profile .. name .. '.code') or ''):match('%d+')
    local out = S.read(profile .. name .. '.out') or ''
    assert(code == '0', name .. ' failed with ' .. tostring(code) .. ':\n' .. out:sub(-3000)
      .. '\nsidecar:\n' .. (S.read(profile .. 'sidecar.out') or ''):sub(-3000))
    -- The activation loaded the campaign's mission from `[missions]`, once.
    assert(#host.loads == loads + 1 and host.loads[#host.loads] == mission,
      name .. ': the activation did not load ' .. mission .. ' once: '
        .. table.concat(host.loads, ', ', loads + 1))
    S.ok(name .. ' ran against nanoprobe-sidecar, its activation loading ' .. mission .. ': '
      .. (out:match('hello campaign done:[^\n]*') or out:sub(-200)))
  end
  -- The health command, against the loopback listener with no tokens file.
  start('health', string.format('%s health --config %s', S.quote(SIDECAR),
    S.quote(profile .. 'sidecar/sidecar.toml')))
  frames_until(function() return S.read(profile .. 'health.code') end, 20, 'the health command')
  local health = S.read(profile .. 'health.out') or ''
  assert((S.read(profile .. 'health.code') or ''):match('%d+') == '0' and health:find('^healthy'),
    'nanoprobe-sidecar health: ' .. health)
  S.ok('nanoprobe-sidecar health: ' .. health:gsub('%s+$', ''))
end)
-- The sidecar stops on SIGTERM. Frames run while it detaches.
if not ok then
  print(err)
end
os.execute('kill ' .. tostring(sidecar_pid))
frames_until(function() return S.read(profile .. 'sidecar.code') end, 20, 'the sidecar stopping')
assert(ok, err)
local code = (S.read(profile .. 'sidecar.code') or ''):match('%d+')
assert(code == '0', 'the sidecar exited with ' .. tostring(code) .. ':\n'
  .. (S.read(profile .. 'sidecar.out') or ''):sub(-3000))
S.ok('nanoprobe-sidecar stopped cleanly on SIGTERM')
