-- The hook, version 0, run in the gui state by the loader with the install
-- root and the active build hash.
--
-- It writes a session marker to dcs.log, loads the agent DLL into this state,
-- logs the mission lifecycle as JSON lines (spec/log.md), and honours the kill
-- switch: once a second it looks for <root>/disable, and from then on does
-- nothing. Later steps add the bootstrap into the server state and the frame
-- pump.
--
-- No callback returns a value: a value returned from an onPlayerTry*
-- callback ends the hook chain for later scripts (ED-CODE,
-- API/Sim_ControlAPI.md).

local root, build = ...
local dir = root .. 'versions/' .. build .. '/'

local logmodule = assert(loadfile(dir .. 'lua/log.lua'))()

local generation = 0
local disabled = false
local next_check = 0

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

-- The agent DLL, opened in this state. Opening pins it, and configuring it
-- opens logs/agent.log and the crash record logs/crash.log. A failure is one
-- record, and the hook carries on without the agent.
local ABI = '1'
local agent
do
  local loaded, result = pcall(function()
    local loader, err = package.loadlib(dir .. 'nanoprobe_agent_' .. build .. '.dll', 'luaopen_nanoprobe')
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
  else
    file.write('error', 'agent not loaded', { error = tostring(result) })
  end
end

local function exists(path)
  local f = io.open(path, 'rb')
  if f then
    f:close()
    return true
  end
  return false
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

local callbacks = {}

callbacks.onMissionLoadBegin = guard('onMissionLoadBegin', function()
  generation = generation + 1
  file.write('info', 'mission load begin', { mission = DCS.getMissionName() })
end)

callbacks.onSimulationStart = guard('onSimulationStart', function()
  file.write('info', 'simulation start', { paused = DCS.getPause() })
end)

callbacks.onSimulationStop = guard('onSimulationStop', function()
  file.write('info', 'simulation stop')
end)

callbacks.onSimulationFrame = guard('onSimulationFrame', function()
  local now = DCS.getRealTime()
  if now < next_check then
    return
  end
  next_check = now + 1
  if exists(root .. 'disable') then
    file.write('warn', 'kill switch found; stopping', { path = root .. 'disable' })
    dcslog(log.WARNING, 'disabled by ' .. root .. 'disable')
    disabled = true
    file.close()
  end
end)

DCS.setUserCallbacks(callbacks)
