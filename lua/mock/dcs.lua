-- Strict mock DCS host for stock Lua 5.1 with no DCS present.
--
-- It gives a test DCS's Lua states as separate global tables: `hook` (where
-- DCS runs the files in Scripts/Hooks), `gui`, `server`, `mission`, `config`,
-- and, while a mission is loaded, `export` and `sse` (the sandboxed mission
-- scripting environment). `hook` and `sse` have no name in DCS. A state holds exactly the globals its listing in
-- lua/mock/api/ names, so `io` is absent where DCS removes it. Every DCS
-- table is strict: reading a name that is not in the listing raises, and
-- calling a listed function the mock does not implement raises a different
-- error. lua/mock/world.lua adds a small simulated world to the states that
-- have the scripting API.
--
-- Quirks it reproduces, each with its evidence tag:
--   - net.dostring_in takes exactly a state name and a string (ED-CODE). It
--     returns two values: a string, and whether the chunk ran. The string is
--     the chunk's first value when that is a string or a number, and empty
--     for any other value. A chunk that fails to compile or raises gives its
--     error text and false; an error value that is not a string gives an
--     empty string (LIVE).
--   - A hook's globals are not the `gui` state's (LIVE, dedicated server).
--   - An unknown state name returns nil; `userhooks` is not a state (LIVE).
--   - `server` and `scripting` are the same state (LIVE).
--   - DCS.setUserCallbacks is additive (LIVE).
--   - A file handle from io.open in the hook state has no `seek`, and its
--     `write` returns nil on success (LIVE).
--   - A timer runs at most once per millisecond of model time, and a frame
--     runs every timer that came due in it, so a timer that asks for less
--     runs in bursts (LIVE).
--   - Timers do not run and model time stands still while paused (LIVE).
--   - A mission load clears timers and keeps event handlers, and model time
--     returns to 0 (LIVE).
--   - No onSimulationFrame arrives during a mission load (LOG).
--   - A mission can start paused with no pause callback, and a resume can
--     arrive with no pause before it (LOG).
--   - DCS.setPause takes effect on a later frame (LIVE).
--   - Event dispatch has no pcall: a handler that raises stops every later
--     handler from seeing that event, and the error reaches the caller
--     (ED-CODE, Scripts/World/EventHandlers.lua). DCS walks the handlers
--     with pairs, in no stated order; the mock walks them in the order they
--     were added, so a test is repeatable.
--   - a_do_script exists only while a mission is loaded (PRIOR), and hands
--     back what the chunk returned shifted by one (LIVE).
-- Unmeasured behaviour is marked UNVERIFIED where the mock picks one.
--
-- Usage:
--   local mock = dofile('lua/mock/dcs.lua')
--   local host = mock.new({ writedir = '/tmp/x/' })
--   host:run('hook', 'path/to/hook.lua')
--   host:load_mission('m')
--   host:frame(1 / 60)

local here = debug.getinfo(1, 'S').source:match('^@(.*[/\\])') or './'
local listing = dofile(here .. 'listing.lua')
local world = dofile(here .. 'world.lua')

local M = {}

M.listing = listing

-- The globals of stock Lua 5.1. A state gets the real one for each of these
-- that its listing names.
local BASE = {}
for _, name in ipairs({
  '_VERSION', 'assert', 'collectgarbage', 'coroutine', 'debug', 'dofile', 'error', 'gcinfo', 'getfenv',
  'getmetatable', 'io', 'ipairs', 'load', 'loadfile', 'loadstring', 'math', 'module', 'newproxy', 'next',
  'os', 'package', 'pairs', 'pcall', 'print', 'rawequal', 'rawget', 'rawset', 'require', 'select',
  'setfenv', 'setmetatable', 'string', 'table', 'tonumber', 'tostring', 'type', 'unpack', 'xpcall',
}) do
  BASE[name] = true
end

local function quote(path)
  return "'" .. path:gsub("'", "'\\''") .. "'"
end

-- Create a host. `o` holds:
--   writedir    the Saved Games profile, with a trailing slash
--   start_time  mission start, seconds after midnight (default 43200)
--   theatre     the map's name (default Caucasus)
function M.new(o)
  o = o or {}
  local host = {
    writedir = o.writedir or './',
    start_time = o.start_time or 43200,
    theatre = o.theatre or 'Caucasus',
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
    -- Each path given to net.load_mission, in order.
    loads = {},
    chats = {},
    -- The connected players by connection ID, as net.get_player_info gives
    -- them (a server player is always ID 1), the ban list, each moderation
    -- call in order, and per-call switches: `net_raises[name]` makes the call
    -- raise and `net_returns[name]` is what it returns.
    players = { [1] = { ucid = 'server-ucid', name = 'server', side = 0, slot = '', ping = 0 } },
    bans = {},
    moderation = {},
    -- The mission's player slots by coalition, as DCS.getAvailableSlots gives
    -- them on the server: every slot, taken ones included (LIVE), and each
    -- read in order by coalition name.
    slots = { red = {}, blue = {} },
    slot_reads = {},
    net_raises = {},
    net_returns = {},
    pending_load = nil,
    timers = {},
    next_timer = 1,
    -- The server state's event handlers, in the order they were added.
    handlers = {},
    states = {},
    listing = listing,
    -- Listed globals a state lacks for now. a_do_script exists only while
    -- a mission is loaded (PRIOR).
    absent = { mission = { a_do_script = true } },
  }

  -- A strict table for the DCS table at `path` in `state`. `impl` holds the
  -- mocked members; a table in it whose members the listing names becomes
  -- strict too. A listed number or boolean reads as its listed value.
  local function namespace(state, path, impl)
    local names = listing.states[state]
    impl = impl or {}
    -- Listed numbers and booleans are real members, so pairs finds them.
    for _, key in ipairs(names.members[path] or {}) do
      local value = names.values[path .. '.' .. key]
      if value ~= nil and rawget(impl, key) == nil then
        rawset(impl, key, value)
      end
    end
    for key, value in pairs(impl) do
      if type(value) == 'table' and getmetatable(value) == nil and names.members[path .. '.' .. key] then
        namespace(state, path .. '.' .. key, value)
      end
    end
    return setmetatable(impl, {
      __index = function(t, key)
        local full = path .. '.' .. tostring(key)
        local where = state .. ' ' .. full
        local kind = names.kinds[full]
        if kind == 'function' then
          -- Present, so a test of its type passes; calling it raises.
          local stub = function()
            error('mock: ' .. where .. ' is in the DCS API but not mocked', 2)
          end
          rawset(t, key, stub)
          return stub
        elseif kind == 'table' then
          local alias = names.alias[full]
          local nested = alias and host.states[state][alias] or namespace(state, full, {})
          rawset(t, key, nested)
          return nested
        elseif kind == 'number' or kind == 'boolean' then
          return names.values[full]
        elseif kind then
          error('mock: ' .. where .. ' is a ' .. kind .. ' the mock does not hold', 2)
        end
        error('mock: ' .. where .. ' is not in the listing for DCS ' .. listing.build, 2)
      end,
    })
  end
  host.namespace = namespace

  -- A state: a global table holding what its listing names. A base-library
  -- name reads through to the real one. A DCS table is strict, and empty
  -- until the mock gives it members.
  local function new_state(name)
    local names = listing.states[name]
    local state
    state = setmetatable({}, {
      __index = function(_, key)
        local kind = names.globals[key]
        if kind == nil or (host.absent[name] and host.absent[name][key]) then
          return nil
        end
        if BASE[key] then
          return _G[key]
        end
        -- A second name for a table follows the first, whatever the mock
        -- puts there, so it is never stored.
        if names.alias[key] then
          return state[names.alias[key]]
        end
        local value
        if kind == 'table' then
          value = namespace(name, key, {})
        elseif kind == 'function' then
          value = function()
            error('mock: ' .. name .. ' ' .. key .. ' is in the DCS API but not mocked', 2)
          end
        elseif kind == 'number' or kind == 'boolean' then
          value = names.values[key]
        else
          value = '<' .. kind .. '>'
        end
        rawset(state, key, value)
        return value
      end,
    })
    state._G = state
    -- Stock dofile and loadfile compile against the real globals, so a file
    -- they ran would set its globals outside the state.
    if names.globals.loadfile then
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
    end
    if names.globals.loadstring then
      state.loadstring = function(s, chunkname)
        local chunk, err = loadstring(s, chunkname)
        if chunk then
          setfenv(chunk, state)
        end
        return chunk, err
      end
    end
    host.states[name] = state
    -- Make every listed global a real member now, so pairs over the state
    -- finds the same names before and after code has read them.
    for key in pairs(names.globals) do
      local _ = state[key]
    end
    return state
  end

  local function make_log(state)
    return namespace(state, 'log', {
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

  -- net.dostring_in returns a string and whether the chunk ran (LIVE).
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
    -- Neither the hook state nor the sandboxed mission scripting
    -- environment has a name here (LIVE).
    local state = target ~= 'sse' and target ~= 'hook' and host.states[target] or nil
    if not state then
      return nil
    end
    local fn, err = loadstring(chunk)
    if not fn then
      return err, false
    end
    setfenv(fn, state)
    local ok, result = pcall(fn)
    local text = (type(result) == 'string' or type(result) == 'number') and tostring(result) or ''
    return text, ok
  end

  -- Real time moves 100 ns on every read, so code that waits for the clock
  -- to change ends.
  local function real_time()
    host.real = host.real + 1e-7
    return host.real
  end

  -- The hook state: where DCS runs the files in Scripts/Hooks.
  local hook = new_state('hook')
  hook.log = make_log('hook')
  hook.lfs = namespace('hook', 'lfs', {
    writedir = function()
      return host.writedir
    end,
    mkdir = function(path)
      if os.execute('mkdir ' .. quote(path) .. ' 2>/dev/null') == 0 then
        return true
      end
      return nil, 'File exists'
    end,
    dir = function(path)
      local names = {}
      local pipe = io.popen('ls -a ' .. quote(path) .. ' 2>/dev/null')
      for name in pipe:lines() do
        names[#names + 1] = name
      end
      pipe:close()
      if #names == 0 then
        error('cannot open ' .. path, 2)
      end
      local i = 0
      return function()
        i = i + 1
        return names[i]
      end
    end,
    -- Only `mode` and `size`. UNVERIFIED: DCS's other fields.
    attributes = function(path, field)
      local f = io.open(path, 'rb')
      if not f then
        return nil
      end
      local attributes = { mode = 'file', size = f:seek('end') }
      f:close()
      if field then
        return attributes[field]
      end
      return attributes
    end,
  })
  -- A file handle in the hook state has no `seek`, and its `write` returns
  -- nil on success (LIVE, DCS 2.9.31.29434 dedicated server under Wine
  -- 10.0). Its other methods, and what `write` returns on an error, are
  -- UNVERIFIED; the mock keeps stock behaviour for them.
  hook.io = setmetatable({
    open = function(path, mode)
      local file, err = io.open(path, mode)
      if not file then
        return nil, err
      end
      local handle = {}
      for _, method in ipairs({ 'read', 'write', 'lines', 'flush', 'close', 'setvbuf' }) do
        handle[method] = function(_, ...)
          local results = { file[method](file, ...) }
          if method == 'write' and results[1] then
            return nil
          end
          if results[1] == file then
            results[1] = handle
          end
          return unpack(results)
        end
      end
      return handle
    end,
  }, { __index = io })
  hook.DCS = namespace('hook', 'DCS', {
    getRealTime = real_time,
    getModelTime = function()
      return host.model
    end,
    getPause = function()
      return host.paused
    end,
    -- Takes effect at the next frame (LIVE). UNVERIFIED: whether the pause
    -- or resume callback follows; the mock sends it.
    setPause = function(paused)
      host.pending_pause = paused and true or false
    end,
    getMissionName = function()
      return host.mission
    end,
    getMissionFilename = function()
      return host.mission_file
    end,
    -- Nil while onSimulationStop runs (PRIOR).
    getMissionTheatre = function()
      if host.stopping or not host.mission then
        return nil
      end
      return host.theatre
    end,
    isServer = function()
      return true
    end,
    isMultiplayer = function()
      return true
    end,
    -- UNVERIFIED: the mock answers nil for an install with nothing tainted.
    getTaintedFiles = function()
      return nil
    end,
    setUserCallbacks = function(callbacks)
      host.callbacks[#host.callbacks + 1] = callbacks
    end,
    -- `net_raises.getAvailableSlots` makes it raise and
    -- `net_returns.getAvailableSlots` is what it returns.
    getAvailableSlots = function(coalition)
      host.slot_reads[#host.slot_reads + 1] = coalition
      -- `net_raises['getAvailableSlots.<coalition>']` raises for that one.
      if host.net_raises.getAvailableSlots or host.net_raises['getAvailableSlots.' .. tostring(coalition)] then
        error('mock: DCS.getAvailableSlots raises', 2)
      end
      if host.net_returns.getAvailableSlots ~= nil then
        return host.net_returns.getAvailableSlots
      end
      local list = {}
      for i, slot in ipairs(host.slots[coalition] or {}) do
        list[i] = { unitId = slot.unitId, type = slot.type or 'A-10C_2', role = slot.role or 'Pilot' }
      end
      return list
    end,
  })
  -- One moderation call: recorded, then raising or returning what the test
  -- set, else run and returning nothing, as the DCS calls return (UNVERIFIED).
  local function moderate(name, args, run)
    host.moderation[#host.moderation + 1] = { call = name, args = args }
    local raises = host.net_raises[name]
    if raises then
      error(type(raises) == 'string' and raises or ('mock: net.' .. name .. ' raises'), 3)
    end
    local forced = host.net_returns[name]
    if forced ~= nil then
      return forced
    end
    return run()
  end

  hook.net = namespace('hook', 'net', {
    dostring_in = dostring_in,
    -- The load happens at the start of the next frame, not inside the call.
    -- It returned 0 for a mission that then loaded, and the load began within
    -- the same second (LIVE, dedicated server). The return for a failed load
    -- is UNVERIFIED.
    load_mission = function(path)
      if type(path) ~= 'string' then
        error('mock: net.load_mission(string) called with ' .. type(path), 2)
      end
      host.loads[#host.loads + 1] = path
      host.pending_load = path
      return 0
    end,
    log = function(message)
      hook.log.write('NET', hook.log.INFO, message)
    end,
    -- Each chat call, in order, as { call, text, all } or { call, text,
    -- player }. A test sets `host.chat_raises` to make one raise.
    send_chat = function(text, all)
      if type(text) ~= 'string' or type(all) ~= 'boolean' then
        error('mock: net.send_chat takes a string and a boolean', 2)
      end
      if host.chat_raises then
        error('mock: net.send_chat raises', 2)
      end
      host.chats[#host.chats + 1] = { call = 'send_chat', text = text, all = all }
    end,
    send_chat_to = function(text, player)
      if type(text) ~= 'string' or type(player) ~= 'number' then
        error('mock: net.send_chat_to takes a string and a connection ID', 2)
      end
      if host.chat_raises then
        error('mock: net.send_chat_to raises', 2)
      end
      host.chats[#host.chats + 1] = { call = 'send_chat_to', text = text, player = player }
    end,
    -- Players and moderation. A call is recorded in `host.moderation` as its
    -- name and arguments before it checks anything else.
    -- `net_raises.get_player_list` makes it raise and
    -- `net_returns.get_player_list` is what it returns.
    get_player_list = function()
      if host.net_raises.get_player_list then
        error('mock: net.get_player_list raises', 2)
      end
      if host.net_returns.get_player_list ~= nil then
        return host.net_returns.get_player_list
      end
      local ids = {}
      for id in pairs(host.players) do
        ids[#ids + 1] = id
      end
      table.sort(ids)
      return ids
    end,
    get_player_info = function(id, attribute)
      if type(id) ~= 'number' or (attribute ~= nil and type(attribute) ~= 'string') then
        error('mock: net.get_player_info takes a connection ID and an optional name', 2)
      end
      -- `net_raises['get_player_info.side']` makes a read of 'side' raise,
      -- and `net_returns['get_player_info.side']` is what it returns.
      local switch = 'get_player_info.' .. tostring(attribute)
      if attribute ~= nil and host.net_raises[switch] then
        error('mock: net.get_player_info(' .. attribute .. ') raises', 2)
      end
      if attribute ~= nil and host.net_returns[switch] ~= nil then
        return host.net_returns[switch]
      end
      local p = host.players[id]
      if p == nil then
        return nil
      end
      if attribute ~= nil then
        return p[attribute]
      end
      local copy = { id = id }
      for k, v in pairs(p) do
        copy[k] = v
      end
      return copy
    end,
    kick = function(id, reason)
      return moderate('kick', { id, reason }, function()
        if type(id) ~= 'number' or type(reason) ~= 'string' then
          error('mock: net.kick takes a connection ID and a string', 3)
        end
        host.players[id] = nil
        host:callback('onPlayerDisconnect', id, 0)
      end)
    end,
    banlist_add = function(id, seconds, reason)
      return moderate('banlist_add', { id, seconds, reason }, function()
        if type(id) ~= 'number' or type(seconds) ~= 'number' or type(reason) ~= 'string' then
          error('mock: net.banlist_add takes a connection ID, seconds and a string', 3)
        end
        local p = host.players[id]
        if not p then
          error('mock: net.banlist_add of a player who is not connected', 3)
        end
        host.bans[#host.bans + 1] = { ucid = p.ucid, name = p.name, reason = reason, ipaddr = p.ipaddr or '10.0.0.9',
          banned_until = seconds == 0 and 4102444800 or host.real + seconds }
      end)
    end,
    banlist_add_by_ucid = function(ucid, seconds, reason)
      return moderate('banlist_add_by_ucid', { ucid, seconds, reason }, function()
        if type(ucid) ~= 'string' or type(seconds) ~= 'number' or type(reason) ~= 'string' then
          error('mock: net.banlist_add_by_ucid takes a ucid, seconds and a string', 3)
        end
        host.bans[#host.bans + 1] = { ucid = ucid, name = '', reason = reason, ipaddr = '10.0.0.9',
          banned_until = seconds == 0 and 4102444800 or host.real + seconds }
      end)
    end,
    banlist_remove = function(ucid)
      return moderate('banlist_remove', { ucid }, function()
        if type(ucid) ~= 'string' then
          error('mock: net.banlist_remove takes a ucid', 3)
        end
        for i = #host.bans, 1, -1 do
          if host.bans[i].ucid == ucid then
            table.remove(host.bans, i)
          end
        end
      end)
    end,
    banlist_get = function()
      host.moderation[#host.moderation + 1] = { call = 'banlist_get' }
      if host.net_raises.banlist_get then
        error('mock: net.banlist_get raises', 2)
      end
      local list = {}
      for i, entry in ipairs(host.bans) do
        local copy = {}
        for k, v in pairs(entry) do
          copy[k] = v
        end
        list[i] = copy
      end
      return list
    end,
    force_player_slot = function(id, side, slot)
      return moderate('force_player_slot', { id, side, slot }, function()
        if type(id) ~= 'number' or type(side) ~= 'number' or type(slot) ~= 'string' then
          error('mock: net.force_player_slot takes a connection ID, a side and a slot string', 3)
        end
        local p = host.players[id]
        if not p then
          error('mock: net.force_player_slot of a player who is not connected', 3)
        end
        -- A slot of the other coalition returns false and changes nothing;
        -- a slot the mission lacks is taken (LIVE,
        -- docs/results/2026-11-24-player-moves.md).
        local other = side == 1 and 'blue' or side == 2 and 'red' or nil
        for _, s in ipairs(other and host.slots[other] or {}) do
          if tostring(s.unitId) == slot then
            return false
          end
        end
        p.side, p.slot = side, slot
        host:callback('onPlayerChangeSlot', id)
        return true
      end)
    end,
  })

  -- The gui state. On a dedicated server its globals are not the hook's: a
  -- global set in one is absent from the other (LIVE).
  local gui = new_state('gui')
  gui.log = make_log('gui')
  gui.lfs = namespace('gui', 'lfs', {
    writedir = function()
      return host.writedir
    end,
  })
  gui.DCS = namespace('gui', 'DCS', {
    getRealTime = real_time,
    getModelTime = function()
      return host.model
    end,
    getPause = function()
      return host.paused
    end,
  })

  -- The server state.
  local server = new_state('server')
  server.log = make_log('server')
  server.lfs = namespace('server', 'lfs', {
    writedir = function()
      return host.writedir
    end,
  })
  server.DCS = namespace('server', 'DCS', {
    getRealTime = real_time,
    getModelTime = function()
      return host.model
    end,
    getPause = function()
      return host.paused
    end,
  })
  server.timer = namespace('server', 'timer', {
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
    -- Removing a timer that has run, or was never scheduled, raises (LIVE).
    removeFunction = function(id)
      if not host.timers[id] then
        error('Parameter #1 (function reference number) is invalid', 2)
      end
      host.timers[id] = nil
    end,
    setFunctionTime = function(id, time)
      if host.timers[id] then
        host.timers[id].time = time
      end
    end,
  })
  -- ED's own Lua, Scripts/World/EventHandlers.lua, defines these three and
  -- the table (ED-CODE).
  local eventHandlers = {}
  server.world = namespace('server', 'world', {
    eventHandlers = eventHandlers,
    addEventHandler = function(handler)
      if not eventHandlers[handler] then
        host.handlers[#host.handlers + 1] = handler
      end
      eventHandlers[handler] = handler
    end,
    removeEventHandler = function(handler)
      eventHandlers[handler] = nil
      for i, h in ipairs(host.handlers) do
        if h == handler then
          table.remove(host.handlers, i)
          return
        end
      end
    end,
    onEvent = function(event)
      for _, handler in ipairs({ unpack(host.handlers) }) do
        handler:onEvent(event)
      end
    end,
  })
  server.env = namespace('server', 'env', {
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

  -- The mission state. `a_do_script` runs a chunk in the sandboxed mission
  -- scripting environment and passes its other arguments to the chunk.
  local mission = new_state('mission')
  local function a_do_script(source, ...)
    local fn, err = loadstring(source)
    if not fn then
      error(err, 2)
    end
    setfenv(fn, host.states.sse)
    local function shift(...)
      local results = { ... }
      return nil, unpack(results, 1, math.max(select('#', ...) - 1, 0))
    end
    return shift(fn(...))
  end

  -- The config state. Neither gate key is set (LIVE on an unconfigured
  -- install), so `net` holds only what the listing names.
  new_state('config')

  world.install(host, 'hook')
  world.install(host, 'server')
  world.install(host, 'mission')

  -- The sandboxed mission scripting environment and the export state are
  -- made again at every mission load (LIVE). net.dostring_in is a function
  -- in the sandbox (LIVE).
  local function new_mission_states()
    local sse = new_state('sse')
    sse.net = namespace('sse', 'net', { dostring_in = dostring_in })
    world.install(host, 'sse')
    new_state('export')
    rawset(mission, 'a_do_script', a_do_script)
  end
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

  -- The values each registered callback returned, one table of results per
  -- callback that has `name`, in registration order.
  function host:callback_values(name, ...)
    local out = {}
    for _, callbacks in ipairs(self.callbacks) do
      local fn = callbacks[name]
      if fn then
        out[#out + 1] = { fn(...) }
      end
    end
    return out
  end

  -- A player connects: `info` holds ucid, name, side and slot. Then
  -- onPlayerConnect(id).
  function host:connect(id, info)
    self.players[id] = {
      ucid = info.ucid, name = info.name, side = info.side or 0, slot = info.slot or '',
      ping = 20, ipaddr = info.ipaddr or '10.0.0.7:10308',
    }
    self:callback('onPlayerConnect', id)
  end

  -- A player changes slot: onPlayerChangeSlot(id) after the info changed.
  function host:change_slot(id, side, slot)
    self.players[id].side, self.players[id].slot = side, slot
    self:callback('onPlayerChangeSlot', id)
  end

  -- A player chats. Returns what each onPlayerTrySendChat returned, so a test
  -- can check none returned a value.
  function host:chat(id, message, all)
    return self:callback_values('onPlayerTrySendChat', id, message, all ~= false)
  end

  -- A player leaves. The mock removes the player before the callback, so
  -- code that relies on net.get_player_info there fails.
  function host:disconnect(id, code)
    self.players[id] = nil
    self:callback('onPlayerDisconnect', id, code or 0)
  end

  -- Load a mission. `name` is a mission name or the path of a .miz.
  -- `options.paused` starts it paused with no pause callback, as a server
  -- with no client does. `options.during`, a function, runs after
  -- onMissionLoadBegin and before the rest of the load, with no frame: a
  -- test spends the load's time there. The callbacks come in the order a dedicated server
  -- made them (LIVE): the server's own player stops and starts with the
  -- mission.
  function host:load_mission(name, options)
    options = options or {}
    if self.mission then
      -- A handler added before the load sees the mission end and the next
      -- one start (LIVE).
      self:event({ id = server.world.event.S_EVENT_MISSION_END, time = self.model })
      self.stopping = true
      self:callback('onSimulationStop')
      self:callback('onPlayerStop', 1)
      self.stopping = false
    end
    self.loading = true
    self.mission_file = name:match('%.miz$') and name or (self.writedir .. 'Missions/' .. name .. '.miz')
    self.mission = name:match('([^/\\]+)%.miz$') or name
    self:callback('onNetMissionChanged', self.mission)
    self:callback('onMissionLoadBegin')
    if options.during then
      options.during()
    end
    self.timers = {}
    -- UNVERIFIED: whether DCS gives a timer an ID that a cleared timer had.
    -- The mock does, because that is the case code must survive.
    self.next_timer = 1
    self.model = 0
    self.sim:clear()
    new_mission_states()
    self:callback('onPlayerStart', 1)
    self:callback('onMissionLoadEnd')
    self.loading = false
    self.paused = options.paused and true or false
    self:callback('onSimulationStart')
    self:event({ id = server.world.event.S_EVENT_MISSION_START, time = 0 })
  end

  -- Pause or resume. `options.silent` skips the callback.
  function host:set_pause(paused, options)
    self.paused = paused and true or false
    if not (options and options.silent) then
      self:callback(paused and 'onSimulationPause' or 'onSimulationResume')
    end
  end

  -- Run every timer that is due, earliest first. A timer that returns a
  -- number runs again at that time, and no sooner than a millisecond after
  -- the time it was due.
  local function run_timers(self)
    for _ = 1, 100000 do
      local next_id
      for id, t in pairs(self.timers) do
        if t.time <= self.model + 1e-9 and (not next_id or t.time < self.timers[next_id].time
          or (t.time == self.timers[next_id].time and id < next_id)) then
          next_id = id
        end
      end
      if not next_id then
        return
      end
      local t = self.timers[next_id]
      self.timers[next_id] = nil
      local due = math.max(t.time, 0)
      -- UNVERIFIED: what DCS does with a raising timer function. The mock
      -- records the error and drops the timer.
      local ok, next_time = pcall(t.fn, t.arg, due)
      if not ok then
        self.timer_errors[#self.timer_errors + 1] = next_time
      elseif type(next_time) == 'number' and self.timers[next_id] == nil then
        t.time = math.max(next_time, due + 0.001)
        self.timers[next_id] = t
      end
    end
    error('mock: a timer ran 100,000 times in one frame')
  end

  -- Advance one frame of `dt` seconds of real time. Model time moves only
  -- when a mission runs and is not paused.
  function host:frame(dt)
    dt = dt or (1 / 60)
    self.real = self.real + dt
    if self.pending_load then
      local path = self.pending_load
      self.pending_load = nil
      self:load_mission(path)
      return
    end
    if self.loading then
      return
    end
    if self.pending_pause ~= nil then
      local paused = self.pending_pause
      self.pending_pause = nil
      if paused ~= self.paused then
        self:set_pause(paused)
      end
    end
    if self.mission and not self.paused then
      self.model = self.model + dt
      self.sim:step(dt)
    end
    if self.mission then
      self.sim:flush(self.paused)
    end
    if self.mission and not self.paused then
      run_timers(self)
    end
    self:callback('onSimulationFrame')
  end

  -- Dispatch an event to the server state's handlers and then the
  -- sandbox's, with no pcall.
  function host:event(event)
    server.world.onEvent(event)
    local sse = self.states.sse
    if sse then
      sse.world.onEvent(event)
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
