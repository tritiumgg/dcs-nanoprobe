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
local ABI = '9'
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

-- Commands whose handler runs in this state (spec/commands.md, "Handlers").
-- Each returns a result code, a value, and a message, as the shim's do, and
-- runs under pcall. The agent has admitted the command.
local APPLIED, NOT_BOUND, DCS_REFUSED, LUA_ERROR = 0, 18, 22, 23

-- The platform's unit name form, at most 28 characters (spec/world.md, "Names").
local UNIT_NAME = '^np%.u%.[0-9a-z]+%.[0-9a-z]+$'
-- The agent formats the life: digits, a point and digits.
local LIFE_TEXT = '^%d+%.%d+$'

-- PICTURE's arguments, as the agent formats them: a number is digits, with a
-- point and digits for a real; a platform name is a group's or a unit's.
local INT_TEXT = '^%d+$'
local NAME_TEXT = '^np%.[gu]%.[0-9a-z]+%.[0-9a-z]+$'
-- A picture's file name goes into Lua source for the mission state, inside
-- double quotes. The agent rejects any name outside this set, which has no
-- quote, backslash or control character, so the name cannot leave the
-- string; the hook checks again because it builds the source. The set is
-- allowed, and the rest is rejected, because no escaping is needed then and
-- none can differ from what the mission state reads.
local FILE_TEXT = '^[%w _%.%-/%(%)%+,#]+$'
-- The mission state's picture function by scope: ALL, COALITION, COUNTRY,
-- GROUP and UNIT (spec/commands.md, "Layout: PICTURE").
local PICTURE_CALL = {
  [1] = 'a_out_picture', [2] = 'a_out_picture_s', [5] = 'a_out_picture_c',
  [3] = 'a_out_picture_g', [4] = 'a_out_picture_u',
}

-- One call into another state. Returns its text, or nil and the reason.
local function cross(state, chunk)
  local text, ok = net.dostring_in(state, chunk)
  if not ok then
    return nil, tostring(text)
  end
  return text
end

local HANDLERS = {
  -- PICTURE: the target's mission ID from the server state for a group or a
  -- unit, then the mission state's picture function from a fixed template
  -- whose every part was checked above or is a number checked here.
  [60] = function(a)
    local scope = type(a.scope) == 'string' and a.scope:find(INT_TEXT) and tonumber(a.scope)
    local call = scope and PICTURE_CALL[scope]
    if not call then
      return DCS_REFUSED, nil, 'the scope is not 1 to 5'
    end
    local file = a.file
    if type(file) ~= 'string' or #file > 255 or not file:find(FILE_TEXT) then
      return DCS_REFUSED, nil, 'the file name is not safe'
    end
    for _, key in ipairs({ 'seconds', 'size' }) do
      if type(a[key]) ~= 'string' or #a[key] > 12 or not a[key]:find(LIFE_TEXT) then
        return DCS_REFUSED, nil, 'the ' .. key .. ' is not formatted'
      end
    end
    for _, key in ipairs({ 'target', 'horizontal', 'vertical', 'size_units' }) do
      if type(a[key]) ~= 'string' or #a[key] > 10 or not a[key]:find(INT_TEXT) then
        return DCS_REFUSED, nil, 'the ' .. key .. ' is not formatted'
      end
    end
    if a.clear_view ~= 'true' and a.clear_view ~= 'false' then
      return DCS_REFUSED, nil, 'clear_view is not true or false'
    end
    -- The target the call names: none, a number, or a mission ID.
    local first = ''
    if scope == 2 or scope == 5 then
      first = a.target .. ', '
    elseif scope == 3 or scope == 4 then
      local name = a.name
      if name == '' then
        return NOT_BOUND, nil, 'no campaign to name the object'
      end
      if type(name) ~= 'string' or #name > 28 or not name:find(NAME_TEXT)
        or name:sub(1, 5) ~= (scope == 3 and 'np.g.' or 'np.u.') then
        return DCS_REFUSED, nil, 'not a platform name'
      end
      local class = scope == 3 and 'Group' or 'Unit'
      local text, err = cross('server', "local o = " .. class .. ".getByName('" .. name .. "') if o then return o:getID() end")
      if not text then
        return DCS_REFUSED, nil, err
      end
      if text == '' then
        return NOT_BOUND, nil, 'no object ' .. name
      end
      local id = tonumber(text)
      if not id or id ~= math.floor(id) or id < 0 or id >= 2 ^ 53 then
        return DCS_REFUSED, nil, 'the object has no integer mission ID'
      end
      first = string.format('%.0f, ', id)
    end
    local text, err = cross('mission', string.format(
      'if not %s then return "none" end %s(%sgetValueResourceByKey("%s"), %s, %s, 0, %s, %s, %s, %s) return "shown"',
      call, call, first, file, a.seconds, a.clear_view,
      a.horizontal, a.vertical, a.size, a.size_units))
    if not text then
      return DCS_REFUSED, nil, err
    elseif text == 'none' then
      return DCS_REFUSED, nil, 'the mission state has no ' .. call
    elseif text ~= 'shown' then
      return DCS_REFUSED, nil, 'the mission state answered ' .. text
    end
    return APPLIED
  end,
  -- PICTURE_STOP
  [61] = function()
    local text, err = cross('mission', 'a_out_picture_stop() return "stopped"')
    if not text then
      return DCS_REFUSED, nil, err
    elseif text ~= 'stopped' then
      return DCS_REFUSED, nil, 'the mission state answered ' .. text
    end
    return APPLIED
  end,
  -- DAMAGE: the unit's mission ID from the server state, then
  -- a_unit_set_life_percentage in the mission state. A unit that is not
  -- there is NOT_BOUND with no mission call: the setter revives a dead unit
  -- (LIVE, E38). A dead unit is NOT_BOUND too.
  [54] = function(a)
    local name, life = a.name, a.life_percent
    if type(name) ~= 'string' or #name > 28 or not name:find(UNIT_NAME) then
      return DCS_REFUSED, nil, 'not a platform unit name'
    end
    if type(life) ~= 'string' or not life:find(LIFE_TEXT) then
      return DCS_REFUSED, nil, 'the life is not formatted'
    end
    local text, err = cross('server', "local u = Unit.getByName('" .. name .. "') if u then return u:getID() end")
    if not text then
      return DCS_REFUSED, nil, err
    end
    if text == '' then
      return NOT_BOUND, nil, 'no unit ' .. name
    end
    local id = tonumber(text)
    if not id or id ~= math.floor(id) or id < 0 or id >= 2 ^ 53 then
      return DCS_REFUSED, nil, 'the unit has no integer mission ID'
    end
    text, err = cross('mission', string.format(
      'if c_unit_dead(%.0f) then return "dead" end a_unit_set_life_percentage(%.0f, %s) return "set"', id, id, life))
    if not text then
      return DCS_REFUSED, nil, err
    elseif text == 'dead' then
      return NOT_BOUND, nil, 'the unit is dead'
    elseif text ~= 'set' then
      return DCS_REFUSED, nil, 'the mission state answered ' .. text
    end
    return APPLIED
  end,
}

-- Run the commands the agent hands out for this state, in ring order, until
-- it has none. Any hook callback can call it; `onSimulationFrame` does when
-- `frame` says a command waits. A command taken and not answered is answered
-- `LUA_ERROR` "no result" by the agent.
local function run_commands()
  while true do
    local opcode, args = agent.command(true)
    if not opcode then
      return
    end
    local handler = HANDLERS[opcode]
    if not handler then
      agent.command_done(20, 'no handler in the hook')
    else
      local ok, code, value, message = pcall(handler, args or {})
      if not ok then
        agent.command_done(LUA_ERROR, tostring(code))
      elseif type(code) ~= 'number' then
        agent.command_done(LUA_ERROR, 'the handler returned no code')
      else
        agent.command_done(code, message, value)
      end
    end
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
    local code, pump, hook = agent.frame(DCS.getRealTime(), DCS.getModelTime(), DCS.getPause(), BY_ID, shim_loaded)
    -- A frame that needs neither a pump nor the hook's commands ends inside
    -- `frame`. Otherwise the pump runs, then the hook's commands, then
    -- `finish`.
    if pump or hook then
      if pump and shim_loaded then
        net.dostring_in('server', PUMP)
      end
      if hook then
        local ran, err = pcall(run_commands)
        if not ran then
          file.write('error', 'hook commands raised', { error = tostring(err) })
        end
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
