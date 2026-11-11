-- Strict mock DCS host, version 0, for stock Lua 5.1 with no DCS present.
--
-- It gives a test two Lua states, `gui` (where hooks run) and `server`, as
-- separate global tables that read through to the real base library. Each
-- DCS namespace in a state is strict: reading a name that is not in
-- lua/mock/listing.lua raises, and reading a listed name the mock does not
-- implement raises a different error.
--
-- Quirks it reproduces, each with its evidence tag:
--   - net.dostring_in takes exactly a state name and a string (ED-CODE).
--   - A chunk that fails to compile or raises returns its error text as an
--     ordinary string (PRIOR, from dcs-bridge; probed in S2).
--   - An unknown state name returns nil; `userhooks` is not a state (LIVE).
--   - `server` and `scripting` are the same state (LIVE).
--   - DCS.setUserCallbacks is additive (LIVE).
--   - Timers run once per frame at most, on a 1 ms model-time quantum (LIVE).
--   - Timers do not run and model time stands still while paused (LIVE).
--   - A mission load clears timers and keeps event handlers, and model time
--     returns to 0 (LIVE).
--   - No onSimulationFrame arrives during a mission load (LOG).
--   - A mission can start paused with no pause callback, and a resume can
--     arrive with no pause before it (LOG).
--   - Event dispatch has no pcall: a handler that raises stops every later
--     handler from seeing that event, and the error reaches the caller
--     (ED-CODE, Scripts/World/EventHandlers.lua).
-- Unmeasured behaviour is marked UNVERIFIED where the mock picks one.
--
-- Usage:
--   local mock = dofile('lua/mock/dcs.lua')
--   local host = mock.new({ writedir = '/tmp/x/' })
--   host:run('gui', 'path/to/hook.lua')
--   host:load_mission('m')
--   host:frame(1 / 60)

local here = debug.getinfo(1, 'S').source:match('^@(.*[/\\])') or './'
local listing = dofile(here .. 'listing.lua')

local M = {}

M.listing = listing

-- A namespace table that raises on a read of a missing name. `impl` holds the
-- mocked members; `names` the listed ones; `extra` constants that are always
-- present. Writes are allowed, as DCS allows them.
local function strict(state, namespace, names, impl)
  local listed = {}
  for _, name in ipairs(names) do
    listed[name] = true
  end
  return setmetatable(impl, {
    __index = function(_, key)
      local where = state .. ' ' .. namespace .. '.' .. tostring(key)
      if listed[key] then
        error('mock: ' .. where .. ' is in the DCS API but not mocked', 2)
      end
      error('mock: ' .. where .. ' is not in the listing for DCS ' .. listing.build, 2)
    end,
  })
end

local function quote(path)
  return "'" .. path:gsub("'", "'\\''") .. "'"
end

-- Create a host. `o` holds:
--   writedir    the Saved Games profile, with a trailing slash
--   start_time  mission start, seconds after midnight (default 43200)
function M.new(o)
  o = o or {}
  local host = {
    writedir = o.writedir or './',
    start_time = o.start_time or 43200,
    real = 0,
    model = 0,
    paused = false,
    loading = false,
    mission = nil,
    -- Every line written through log.write in any state, in order.
    dcslog = {},
    -- Each table handed to DCS.setUserCallbacks, in order.
    callbacks = {},
    -- Errors raised by timer functions, in order.
    timer_errors = {},
    timers = {},
    next_timer = 1,
    handlers = {},
    states = {},
  }

  local function new_state(name)
    local state = setmetatable({}, { __index = _G })
    state._G = state
    -- Stock dofile and loadfile compile against the real globals, so a file
    -- they ran would set its globals outside the state.
    state.loadfile = function(path)
      local chunk, err = loadfile(path)
      if chunk then
        setfenv(chunk, state)
      end
      return chunk, err
    end
    state.dofile = function(path)
      return assert(state.loadfile(path))()
    end
    state.loadstring = function(s, chunkname)
      local chunk, err = loadstring(s, chunkname)
      if chunk then
        setfenv(chunk, state)
      end
      return chunk, err
    end
    host.states[name] = state
    return state
  end

  local function make_log(state)
    return strict(state, 'log', listing[state].log, {
      -- PRIOR values from the listing.
      ALERT = 2,
      ERROR = 8,
      WARNING = 16,
      INFO = 64,
      DEBUG = 128,
      write = function(subsystem, level, message)
        host.dcslog[#host.dcslog + 1] = {
          state = state,
          subsystem = subsystem,
          level = level,
          message = message,
        }
      end,
    })
  end

  local function dostring_in(target, chunk, ...)
    if select('#', ...) > 0 then
      error('mock: net.dostring_in takes a state name and a string, and no more', 2)
    end
    if type(target) ~= 'string' or type(chunk) ~= 'string' then
      error('mock: net.dostring_in(state, string) called with '
        .. type(target) .. ', ' .. type(chunk), 2)
    end
    if target == 'scripting' then
      target = 'server'
    end
    local state = host.states[target]
    if not state then
      if target == 'mission' or target == 'export' or target == 'config' then
        error('mock: the ' .. target .. ' state is not mocked', 2)
      end
      return nil
    end
    local fn, err = loadstring(chunk)
    if not fn then
      return err
    end
    setfenv(fn, state)
    local ok, result = pcall(fn)
    if not ok then
      return tostring(result)
    end
    -- UNVERIFIED: the return of a non-string, and of nil. S2 probes both.
    if result == nil then
      return nil
    end
    return tostring(result)
  end

  -- The gui state.
  local gui = new_state('gui')
  gui.log = make_log('gui')
  gui.lfs = strict('gui', 'lfs', listing.gui.lfs, {
    writedir = function()
      return host.writedir
    end,
    mkdir = function(path)
      if os.execute('mkdir ' .. quote(path) .. ' 2>/dev/null') == 0 then
        return true
      end
      return nil, 'File exists'
    end,
  })
  gui.DCS = strict('gui', 'DCS', listing.gui.DCS, {
    getRealTime = function()
      return host.real
    end,
    getModelTime = function()
      return host.model
    end,
    getPause = function()
      return host.paused
    end,
    getMissionName = function()
      return host.mission
    end,
    isServer = function()
      return true
    end,
    setUserCallbacks = function(callbacks)
      host.callbacks[#host.callbacks + 1] = callbacks
    end,
  })
  gui.net = strict('gui', 'net', listing.gui.net, {
    dostring_in = dostring_in,
    log = function(message)
      gui.log.write('NET', gui.log.INFO, message)
    end,
  })

  -- The server state.
  local server = new_state('server')
  server.log = make_log('server')
  server.DCS = strict('server', 'DCS', listing.server.DCS, {
    getRealTime = function()
      return host.real
    end,
    getModelTime = function()
      return host.model
    end,
    getPause = function()
      return host.paused
    end,
  })
  server.timer = strict('server', 'timer', listing.server.timer, {
    getTime = function()
      return host.model
    end,
    getAbsTime = function()
      return host.start_time + host.model
    end,
    getTime0 = function()
      return host.start_time
    end,
    scheduleFunction = function(fn, arg, time)
      if type(fn) ~= 'function' or type(time) ~= 'number' then
        error('mock: timer.scheduleFunction(function, arg, number)', 2)
      end
      local id = host.next_timer
      host.next_timer = id + 1
      host.timers[id] = { fn = fn, arg = arg, time = time }
      return id
    end,
    removeFunction = function(id)
      host.timers[id] = nil
    end,
    setFunctionTime = function(id, time)
      if host.timers[id] then
        host.timers[id].time = time
      end
    end,
  })
  server.world = strict('server', 'world', listing.server.world, {
    addEventHandler = function(handler)
      host.handlers[#host.handlers + 1] = handler
    end,
    removeEventHandler = function(handler)
      for i, h in ipairs(host.handlers) do
        if h == handler then
          table.remove(host.handlers, i)
          return
        end
      end
    end,
  })
  server.env = strict('server', 'env', listing.server.env, {
    info = function(message)
      server.log.write('SCRIPTING', server.log.INFO, message)
    end,
    warning = function(message)
      server.log.write('SCRIPTING', server.log.WARNING, message)
    end,
    error = function(message)
      server.log.write('SCRIPTING', server.log.ERROR, message)
    end,
    getMissionName = function()
      return host.mission
    end,
  })
  -- `net` is nil in the server state (PRIOR, listing).

  -- Run a file in a state, with arguments. Returns what it returns.
  function host:run(state, path, ...)
    local chunk = assert(self.states[state].loadfile(path))
    return chunk(...)
  end

  -- Call `name` on every registered callback table, in registration order.
  -- An error reaches the caller. UNVERIFIED: what DCS does with a raising
  -- hook callback; the mock lets it fail the test.
  function host:callback(name, ...)
    for _, callbacks in ipairs(self.callbacks) do
      local fn = callbacks[name]
      if fn then
        fn(...)
      end
    end
  end

  -- Load a mission. `options.paused` starts it paused with no pause
  -- callback, as a server with pause_on_load does.
  function host:load_mission(name, options)
    options = options or {}
    if self.mission then
      self:callback('onSimulationStop')
    end
    self.loading = true
    self.mission = name
    self:callback('onMissionLoadBegin')
    self.timers = {}
    self.model = 0
    self:callback('onMissionLoadEnd')
    self.loading = false
    self.paused = options.paused and true or false
    self:callback('onSimulationStart')
  end

  -- Pause or resume. `options.silent` skips the callback.
  function host:set_pause(paused, options)
    self.paused = paused and true or false
    if not (options and options.silent) then
      self:callback(paused and 'onSimulationPause' or 'onSimulationResume')
    end
  end

  -- Advance one frame of `dt` seconds of real time. Model time moves only
  -- when a mission runs and is not paused. Due timers run once each, in
  -- schedule order; a timer that returns a number is rescheduled for that
  -- time, at the earliest the next frame.
  function host:frame(dt)
    dt = dt or (1 / 60)
    self.real = self.real + dt
    if self.loading then
      return
    end
    if self.mission and not self.paused then
      self.model = self.model + dt
      local due = {}
      for id, t in pairs(self.timers) do
        if t.time <= self.model + 0.0005 then
          due[#due + 1] = id
        end
      end
      table.sort(due)
      for _, id in ipairs(due) do
        local t = self.timers[id]
        if t then
          self.timers[id] = nil
          -- UNVERIFIED: what DCS does with a raising timer function. The
          -- mock records the error and drops the timer.
          local ok, next_time = pcall(t.fn, t.arg, self.model)
          if not ok then
            self.timer_errors[#self.timer_errors + 1] = next_time
          elseif type(next_time) == 'number' then
            t.time = math.max(next_time, self.model + 0.001)
            self.timers[id] = t
          end
        end
      end
    end
    self:callback('onSimulationFrame')
  end

  -- Dispatch an event to the server state's handlers, with no pcall.
  function host:event(event)
    for _, handler in ipairs(self.handlers) do
      handler:onEvent(event)
    end
  end

  -- The number of scheduled timers.
  function host:timer_count()
    local n = 0
    for _ in pairs(self.timers) do
      n = n + 1
    end
    return n
  end

  return host
end

return M
