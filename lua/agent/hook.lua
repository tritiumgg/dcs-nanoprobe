-- The hook, run in the hook state by the loader with the install root and the
-- active build hash.
--
-- It writes a session marker to dcs.log, loads the agent DLL into this state,
-- opens the agent's link to the sidecar (spec/transport.md), and pumps it on
-- every frame: the agent's frame, on a sampling frame one cross-state call
-- into the shim's pump, then the agent's finish. On each simulation start it
-- loads the shim into the server state, once per process, and starts it. It
-- logs the mission lifecycle as JSON lines (spec/log.md), which also reach
-- the sidecar through the agent.
--
-- With the link open, the agent looks for the kill switch and the shim reload
-- request on the frames where it makes its system calls. Without it, the hook
-- looks for the kill switch itself, once a second.
--
-- No callback returns a value: a value returned from an onPlayerTry*
-- callback ends the hook chain for later scripts (ED-CODE,
-- API/Sim_ControlAPI.md).

local root, build = ...
local dir = root .. 'versions/' .. build .. '/'

local logmodule = assert(loadfile(dir .. 'lua/log.lua'))()

-- The agent's interface version this hook was written for.
local ABI = '6'
-- What agent.frame returns.
local FRAME_RELOAD, FRAME_DISABLED = 1, 2

local generation = 0
local disabled = false
local next_check = 0
local agent
local linked = false
local dll = dir .. 'nanoprobe_agent_' .. build .. '.dll'

local function dcslog(level, message)
  pcall(log.write, 'NANOPROBE', level, message)
end

local file = logmodule.open({
  path = root .. 'logs/hook.log',
  target = 'hook',
  real = DCS.getRealTime,
  model = DCS.getModelTime,
  gen = function()
    return generation
  end,
  build = build,
  size = function(path)
    return lfs.attributes(path, 'size')
  end,
  fallback = function(message)
    dcslog(log.ERROR, message)
  end,
})

-- The session marker. A bundle takes the part of dcs.log after the last one.
-- The ID mixes wall time and CPU time; it does not reseed math.random, which
-- every hook in the gui state shares.
local session = string.format('%08x%06x', os.time(), math.floor(os.clock() * 1e6) % 0x1000000)
dcslog(log.INFO, 'session ' .. session .. ' build ' .. build)
file.write('info', 'hook loaded', { session = session })

-- The build this process loaded, so a deploy keeps its folder while its DLL
-- is loaded (spec/install.md, "Operations").
pcall(function()
  local f = io.open(root .. 'running.txt', 'wb')
  if f then
    f:write('build=' .. build .. '\n')
    f:close()
  end
end)

-- The DCS version, read once. A missing global reads as nil.
local dcs_version = ''
do
  local ok, v = pcall(function()
    return __DCS_VERSION__
  end)
  if ok and type(v) == 'string' then
    dcs_version = v
  end
end

-- Whether each dotted name is a function or a value in this state, without
-- raising for a name that is absent.
local function probe(names)
  local found = {}
  for _, name in ipairs(names) do
    local ok, present = pcall(function()
      local v = _G
      for part in name:gmatch('[^.]+') do
        if type(v) ~= 'table' then
          return false
        end
        v = v[part]
      end
      return v ~= nil
    end)
    found[name] = ok and present or false
  end
  return found
end

-- What the platform calls in the hook state (spec/records.md,
-- "Capabilities").
local CAPABILITIES = {
  'DCS.getRealTime',
  'DCS.getModelTime',
  'DCS.getPause',
  'DCS.getMissionFilename',
  'DCS.getMissionName',
  'DCS.getMissionTheatre',
  'DCS.setUserCallbacks',
  'net.dostring_in',
  'net.load_mission',
  'lfs.writedir',
  'lfs.attributes',
  'log.write',
  'package.loadlib',
}

-- The agent DLL, opened in this state. Opening pins it, and configuring it
-- opens logs/agent.log and the crash record logs/crash.log. Starting it opens
-- the link. A failure is one record, and the hook carries on without the
-- agent or without the link.
do
  local loaded, result = pcall(function()
    local loader, err = package.loadlib(dll, 'luaopen_nanoprobe')
    if not loader then
      error(err, 0)
    end
    local opened = loader()
    if type(opened) ~= 'table' then
      error('the opener returned a ' .. type(opened), 0)
    end
    if opened.abi ~= ABI then
      error('the DLL has ABI ' .. tostring(opened.abi) .. '; this hook needs ' .. ABI, 0)
    end
    local configured, why = opened.configure(root)
    if not configured then
      error(why, 0)
    end
    return opened
  end)
  if loaded then
    agent = result
    local status = agent.status()
    file.write('info', 'agent loaded', {
      abi = agent.abi,
      pinned = status.pinned,
      pdb = status.pdb,
      thread = status.thread,
    })
    local started, path = agent.start(dcs_version)
    if started then
      linked = true
      -- Every later hook record also goes to the sidecar.
      file.forward = function(level, msg, data)
        local args = {}
        for k, v in pairs(data or {}) do
          args[#args + 1] = tostring(k)
          args[#args + 1] = v
        end
        agent.log(level, msg, 'hook', unpack(args))
      end
      file.write('info', 'link open', { map = path, dcs = dcs_version })
    else
      file.write('error', 'link not opened', { error = tostring(path) })
    end
  else
    file.write('error', 'agent not loaded', { error = tostring(result) })
  end
end

file.write('info', 'capabilities', probe(CAPABILITIES))

local function exists(path)
  local f = io.open(path, 'rb')
  if f then
    f:close()
    return true
  end
  return false
end

local function stop(message, path)
  file.write('warn', message, { path = path })
  dcslog(log.WARNING, 'disabled by ' .. path)
  disabled = true
  file.close()
end

local function guard(name, fn)
  return function(...)
    if disabled then
      return
    end
    local ok, err = pcall(fn, ...)
    if not ok then
      file.write('error', name .. ' raised', { error = tostring(err) })
    end
  end
end

-- The mission's file name without its folder, or ''.
local function mission_file()
  local path = DCS.getMissionFilename()
  if type(path) ~= 'string' then
    return ''
  end
  return path:match('([^/\\]+)$') or path
end

-- Load the shim into the server state behind a guard, so it loads once per
-- process, and start it. `reload` runs the shim at `shim` again over the one
-- loaded, keeping the agent it holds. Returns what the chunk returned and
-- whether it ran.
local BOOT = [[
local dll, shim, root, dir = %q, %q, %q, %q
if not __nanoprobe_shim then
  local loader, err = package.loadlib(dll, 'luaopen_nanoprobe')
  if not loader then
    error(err, 0)
  end
  local body = assert(loadfile(shim))
  __nanoprobe_shim = body(loader(), root, dir, nil)
end
return __nanoprobe_shim.start()
]]

local RELOAD = [[
local shim, root, dir = %q, %q, %q
local old = __nanoprobe_shim
if not old then
  error('no shim is loaded', 0)
end
local body = assert(loadfile(shim))
local new = body(old.agent, root, dir, old)
__nanoprobe_shim = new
return new.start()
]]

-- Whether a shim is loaded in the server state, for the pump.
local shim_loaded = false

-- The pump, one cross-state call on each sampling frame
-- (spec/sampling.md, "The frame"). The string is fixed, so no frame builds
-- one, and short, because net.dostring_in compiles it on every call: a
-- chunk with a local and a test costs about 10 µs more (LIVE, S10). The hook
-- calls it only while a shim is loaded.
local PUMP = '__nanoprobe_shim.pump()'

local function shim_start()
  local chunk = string.format(BOOT, dll, dir .. 'lua/shim.lua', root, dir)
  local text, ok = net.dostring_in('server', chunk)
  if ok then
    shim_loaded = true
    file.write('info', 'shim started', { result = text })
  else
    file.write('error', 'shim not started', { error = tostring(text) })
  end
end

-- Reload the shim of the version the manifest names now, which may be a
-- newer deploy than this hook's. The DLL stays the one loaded.
local function shim_reload()
  local m = io.open(root .. 'manifest.txt', 'rb')
  local active = m and m:read('*a'):match('active=([%w]+)')
  if m then
    m:close()
  end
  if not active then
    file.write('error', 'shim not reloaded', { error = 'no active version in the manifest' })
    return
  end
  local shimdir = root .. 'versions/' .. active .. '/'
  local chunk = string.format(RELOAD, shimdir .. 'lua/shim.lua', root, shimdir)
  local text, ok = net.dostring_in('server', chunk)
  if ok then
    file.write('info', 'shim reloaded', { version = active, result = text })
  else
    file.write('error', 'shim not reloaded', { version = active, error = tostring(text) })
  end
end

-- The position getter by object ID, when this DCS has it in this state: the
-- agent sweeps the units' positions with it inside `frame`, with no
-- cross-state call (spec/sampling.md, "The frame").
local BY_ID
do
  local ok, f = pcall(function()
    return DCS.getUnitPositionByObjectId
  end)
  if ok and type(f) == 'function' then
    BY_ID = f
  end
end

local callbacks = {}

callbacks.onMissionLoadBegin = guard('onMissionLoadBegin', function()
  generation = generation + 1
  if linked then
    generation = agent.lifecycle('loading', '', '', DCS.getRealTime(), DCS.getModelTime()) or generation
  end
  file.write('info', 'mission load begin', { mission = DCS.getMissionName() })
end)

callbacks.onSimulationStart = guard('onSimulationStart', function()
  local paused = DCS.getPause()
  if linked then
    local ok, theatre = pcall(DCS.getMissionTheatre)
    theatre = ok and type(theatre) == 'string' and theatre or ''
    agent.lifecycle(paused and 'paused' or 'running', theatre, mission_file(), DCS.getRealTime(), DCS.getModelTime())
  end
  file.write('info', 'simulation start', { paused = paused })
  if agent then
    shim_start()
  end
end)

callbacks.onSimulationStop = guard('onSimulationStop', function()
  if linked then
    agent.lifecycle('stopping', '', mission_file(), DCS.getRealTime(), DCS.getModelTime())
  end
  file.write('info', 'simulation stop')
end)

callbacks.onSimulationFrame = guard('onSimulationFrame', function()
  if linked then
    local code, pump = agent.frame(DCS.getRealTime(), DCS.getModelTime(), DCS.getPause(), BY_ID)
    -- A frame that does not sample ends inside `frame`.
    if pump then
      if shim_loaded then
        net.dostring_in('server', PUMP)
      end
      agent.finish()
    end
    if code == nil then
      -- A caught panic disabled the agent: every entry returns nothing. The
      -- hook goes on without the link.
      linked = false
      file.write('error', 'the agent is disabled; the hook goes on without it', agent.status())
    elseif code == FRAME_DISABLED then
      stop('kill switch found; stopping', root .. 'disable')
    elseif code == FRAME_RELOAD then
      shim_reload()
    end
    return
  end
  local now = DCS.getRealTime()
  if now < next_check then
    return
  end
  next_check = now + 1
  if exists(root .. 'disable') then
    stop('kill switch found; stopping', root .. 'disable')
  end
end)

DCS.setUserCallbacks(callbacks)
