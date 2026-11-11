-- The eval hook. Development only; never part of a release. `cargo xtask
-- deploy --dev` installs it as <profile>/Scripts/Hooks/nanoprobe-eval.lua.
-- spec/eval.md specifies the channel.
--
-- It runs a dropped Lua file in a chosen state and writes what it returned as
-- JSON. It is pure Lua and depends on no other part of the platform. It also
-- keeps a log of the hook callbacks DCS makes, which a snippet in the hook
-- state reads as E.hook.
--
-- It honours the kill switch. No callback returns a value, and it registers no
-- onPlayerTry* callback: a value returned from one ends the hook chain for
-- later scripts (ED-CODE, API/Sim_ControlAPI.md).

local ok, err = pcall(function()
  local root = lfs.writedir() .. 'nanoprobe/'
  local dev = root .. 'dev/'
  local base = dev .. 'eval/'
  local NAME = '__nanoprobe_eval'
  -- How much of a reply one cross-state call carries back. The largest
  -- string net.dostring_in returns whole is UNVERIFIED, so replies cross in
  -- pieces.
  local PIECE = 16384
  -- Reset order: `sse` is reached through `mission`, and `hook` holds the
  -- encoder the hook itself uses.
  local STATES = { 'server', 'export', 'config', 'sse', 'mission', 'gui', 'hook' }

  local function dcslog(level, message)
    pcall(log.write, 'NANOPROBE-EVAL', level, message)
  end

  local function exists(path)
    local f = io.open(path, 'rb')
    if f then
      f:close()
      return true
    end
    return false
  end

  if exists(root .. 'disable') then
    dcslog(log.INFO, 'disabled by ' .. root .. 'disable; nothing loaded')
    return
  end

  local function read(path)
    local f, why = io.open(path, 'rb')
    if not f then
      return nil, why
    end
    local text = f:read('*a')
    f:close()
    return text
  end

  -- Write under another name and rename into place, so a reader never sees
  -- half a file. A handle's write returns nil on success here (LIVE), so its
  -- result says nothing.
  local function write(path, text)
    local partial = path .. '.partial'
    local f, why = io.open(partial, 'wb')
    if not f then
      return nil, why
    end
    f:write(text)
    f:close()
    os.remove(path)
    return os.rename(partial, path)
  end

  assert(read(dev .. 'lua/prelude.lua'))

  local function q(s)
    return string.format('%q', s)
  end

  local generation = 0
  local NP

  -- Run `chunk` in `state` and return what came back. `hook` is this state,
  -- so the chunk runs here. On a dedicated server it is not the state
  -- net.dostring_in reaches as `gui` (LIVE).
  local function send(state, chunk)
    if state == 'hook' then
      local fn, why = loadstring(chunk)
      if not fn then
        return 'NPE:X:' .. tostring(why)
      end
      local ran, reply = pcall(fn)
      return ran and reply or ('NPE:X:' .. tostring(reply))
    end
    if state == 'sse' then
      state = 'mission'
      chunk = 'local NP = rawget(_G, ' .. q(NAME) .. ') if not NP then return "NPE:M" end return NP.sse('
        .. q(chunk) .. ')'
    end
    local ran, reply = pcall(net.dostring_in, state, chunk)
    if not ran then
      return 'NPE:X:net.dostring_in raised: ' .. tostring(reply)
    end
    return reply
  end

  local install

  -- Call `NP.<expr>` in `state`. With `must`, a state that lacks the prelude
  -- gets it first.
  local function call(state, expr, must)
    local chunk = 'local NP = rawget(_G, ' .. q(NAME) .. ') if not NP then return "NPE:N" end return NP.' .. expr
    local reply = send(state, chunk)
    if must and (reply == 'NPE:N' or reply == 'NPE:M') then
      local installed, why = install(state)
      if not installed then
        return 'NPE:X:could not install the prelude in ' .. state .. ': ' .. tostring(why)
      end
      reply = send(state, chunk)
    end
    return reply
  end

  local hooklog

  -- Run `source` in another state now, for a hook snippet's E.cross.
  local function cross(state, source)
    if type(state) ~= 'string' or type(source) ~= 'string' then
      error('E.cross(state, source) takes two strings', 3)
    end
    local reply = call(state, string.format('plain(%s, %s, %d)', q('cross'), q(source), generation), true)
    if type(reply) == 'string' and reply:sub(1, 6) == 'NPE:V:' then
      return true, reply:sub(7)
    end
    if type(reply) == 'string' and reply:sub(1, 6) == 'NPE:X:' then
      return false, reply:sub(7)
    end
    return false, 'reply of type ' .. type(reply) .. ': ' .. tostring(reply):sub(1, 300)
  end

  function install(state)
    if state == 'sse' then
      local reply = send('mission', 'return rawget(_G, ' .. q(NAME) .. ') and "NPE:I" or "NPE:N"')
      if reply ~= 'NPE:I' then
        local installed, why = install('mission')
        if not installed then
          return nil, why
        end
      end
    end
    -- Read at every install, so a prelude deployed while DCS runs is used
    -- after the next uninstall.
    local prelude, unread = read(dev .. 'lua/prelude.lua')
    if not prelude then
      return nil, unread
    end
    local reply = send(state, 'return (function(...)\n' .. prelude .. '\nend)(' .. q(state) .. ', ' .. q(dev) .. ')')
    if reply ~= 'NPE:I' then
      return nil, 'reply of type ' .. type(reply) .. ': ' .. tostring(reply):sub(1, 300)
    end
    if state == 'hook' then
      NP = rawget(_G, NAME)
      NP.cross = cross
      NP.hook = hooklog
    end
    return true
  end

  -------------------------------------------------------------------------
  -- The callback log.

  local LOG_CAP = 1000
  local entries, first, seq = {}, 1, 0
  local counts = {}
  local frames = { total = 0, paused = 0, loading = 0 }
  local loading = false

  hooklog = {
    seq = function()
      return seq
    end,
    since = function(after)
      local out = {}
      for i = math.max(first, (after or 0) + 1), seq do
        out[#out + 1] = entries[i]
      end
      return out
    end,
    counts = function()
      local out = { frames = frames.total, frames_paused = frames.paused, frames_loading = frames.loading }
      for name, n in pairs(counts) do
        out[name] = n
      end
      return out
    end,
  }

  -- What a getter gives, or nil when it is absent or raises.
  local function try(name)
    local got, value = pcall(function()
      return DCS[name]()
    end)
    if got then
      return value
    end
    return nil
  end

  -- Record a callback: its name, the clocks, and the types of its arguments.
  -- No argument's value is kept, because some hold player names and
  -- addresses. Returns nothing.
  local function note(name, ...)
    counts[name] = (counts[name] or 0) + 1
    local types = {}
    for i = 1, select('#', ...) do
      types[i] = type((select(i, ...)))
    end
    seq = seq + 1
    entries[seq] = {
      seq = seq,
      name = name,
      real = try('getRealTime'),
      model = try('getModelTime'),
      paused = try('getPause'),
      gen = generation,
      args = table.concat(types, ','),
      theatre = try('getMissionTheatre'),
      mission_name = type(try('getMissionName')),
      mission_file = type(try('getMissionFilename')),
    }
    if seq - first >= LOG_CAP then
      entries[first] = nil
      first = first + 1
    end
  end

  -------------------------------------------------------------------------
  -- Requests.

  local current
  local next_poll, next_disable = 0, 0
  local stopped = false

  local function hook_facts(request, now)
    return string.format(
      '{"real_start":%.6f,"real_end":%.6f,"frames":%d,"gen":%d,"paused":%s,"model":%.6f,"resumes":%d}',
      request.started, now, request.frames, generation, tostring(try('getPause') == true),
      tonumber(try('getModelTime')) or 0, request.resumes)
  end

  local function finish(request, reply, problem)
    local now = DCS.getRealTime()
    local parts = {
      '"id":' .. NP.json(request.id),
      '"state":' .. NP.json(request.state),
      '"ok":' .. tostring(problem == nil),
    }
    if problem then
      parts[#parts + 1] = '"error":' .. NP.json(problem)
    else
      parts[#parts + 1] = '"reply":' .. reply
    end
    parts[#parts + 1] = '"hook":' .. hook_facts(request, now)
    local wrote, why = write(base .. 'out/' .. request.id .. '.json', '{' .. table.concat(parts, ',') .. '}\n')
    if not wrote then
      dcslog(log.ERROR, 'could not write the result of ' .. request.id .. ': ' .. tostring(why))
    end
    dcslog(problem and log.WARNING or log.INFO, string.format('%s in %s: %s, %d frame(s)', request.id,
      request.state, problem and ('failed: ' .. problem:sub(1, 200)) or 'done', request.frames))
    current = nil
  end

  -- Take a stored reply of `length` bytes out of `state`, piece by piece.
  local function fetch(state, id, length)
    local pieces, got = {}, 0
    while got < length do
      local piece = call(state, string.format('take(%s, %d, %d)', q(id), got + 1, got + PIECE))
      if type(piece) ~= 'string' or #piece == 0 then
        return nil, 'a piece of the reply did not arrive: ' .. type(piece)
      end
      pieces[#pieces + 1] = piece
      got = got + #piece
    end
    call(state, string.format('drop(%s)', q(id)))
    if got ~= length then
      return nil, 'the reply is ' .. got .. ' bytes, not the ' .. length .. ' announced'
    end
    return table.concat(pieces)
  end

  -- Act on a reply code from run or poll.
  local function handle(request, reply)
    if reply == 'NPE:W' then
      return
    end
    if reply == 'NPE:R' then
      -- The snippet waits on model time. A server pauses when its last
      -- client leaves, and a bare connection to the game port counts as a
      -- client (LOG), so the pause can return at any moment.
      local now = DCS.getRealTime()
      if try('getPause') == true and now >= (request.resume_at or 0) then
        request.resume_at = now + 2
        request.resumes = request.resumes + 1
        DCS.setPause(false)
      end
      return
    end
    if type(reply) ~= 'string' then
      return finish(request, nil, 'the reply is a ' .. type(reply) .. ', not a string')
    end
    local length = reply:match('^NPE:D:(%d+)$')
    if not length then
      return finish(request, nil, reply:sub(1, 6) == 'NPE:X:' and reply:sub(7) or ('unexpected reply: '
        .. reply:sub(1, 300)))
    end
    local text, why = fetch(request.state, request.id, tonumber(length))
    if not text then
      return finish(request, nil, why)
    end
    finish(request, text)
  end

  -- Run every cleanup in every state that holds the prelude. With `remove`,
  -- take the prelude out afterwards.
  local function reset(request, remove)
    local reports = {}
    for _, state in ipairs(STATES) do
      local reply = call(state, string.format('reset(%s)', q(request.id)))
      local length = type(reply) == 'string' and reply:match('^NPE:D:(%d+)$')
      if length then
        local text, why = fetch(state, request.id, tonumber(length))
        reports[#reports + 1] = text or ('{"state":' .. NP.json(state) .. ',"cleaned":{},"errors":['
          .. NP.json(why) .. '],"new_globals":[],"handlers":null}')
        if remove and state ~= 'hook' then
          call(state, 'uninstall()')
        end
      elseif reply ~= 'NPE:N' and reply ~= 'NPE:M' and reply ~= nil then
        reports[#reports + 1] = '{"state":' .. NP.json(state) .. ',"cleaned":{},"errors":['
          .. NP.json(tostring(reply):sub(1, 300)) .. '],"new_globals":[],"handlers":null}'
      end
    end
    finish(request, '[' .. table.concat(reports, ',') .. ']')
    if remove then
      -- The hook keeps `NP` for its encoder until the next install.
      call('hook', 'uninstall()')
    end
  end

  local function begin(name, now)
    local id, state = name:match('^([%w_%-]+)%.(%a+)%.lua$')
    local taken = base .. 'taken/' .. name
    os.remove(taken)
    local renamed, why = os.rename(base .. 'in/' .. name, taken)
    if not renamed then
      dcslog(log.ERROR, 'could not take ' .. name .. ': ' .. tostring(why))
      stopped = true
      return
    end
    if not rawget(_G, NAME) then
      local installed, problem = install('hook')
      if not installed then
        dcslog(log.ERROR, 'could not install the prelude in the hook state: ' .. tostring(problem))
        stopped = true
        return
      end
    end
    local request = { id = id, state = state, started = now, frames = 0, resumes = 0 }
    current = request
    if state == 'reset' or state == 'uninstall' then
      return reset(request, state == 'uninstall')
    end
    local source, problem = read(taken)
    if not source then
      return finish(request, nil, 'could not read the request: ' .. tostring(problem))
    end
    handle(request, call(state, string.format('run(%s, %s, %d, %.6f)', q(id), q(source), generation, now), true))
  end

  -- The request in `in/` whose name sorts first.
  local function waiting()
    local best
    local listed = pcall(function()
      for name in lfs.dir(base .. 'in') do
        if name:match('^[%w_%-]+%.%a+%.lua$') and #name <= 80 and (not best or name < best) then
          best = name
        end
      end
    end)
    return listed and best or nil
  end

  local function frame()
    local now = DCS.getRealTime()
    if current then
      current.frames = current.frames + 1
      handle(current, call(current.state, string.format('poll(%s, %d, %.6f)', q(current.id), generation, now)))
      return
    end
    if now < next_poll then
      return
    end
    next_poll = now + 0.25
    if now >= next_disable then
      next_disable = now + 1
      if exists(root .. 'disable') then
        dcslog(log.WARNING, 'disabled by ' .. root .. 'disable')
        stopped = true
        return
      end
    end
    local name = waiting()
    if name then
      begin(name, now)
    end
  end

  -------------------------------------------------------------------------
  -- Callbacks. The names are the ones ED's own MissionEditor/GameGUI.lua
  -- handles (ED-CODE).

  local callbacks = {}

  for _, name in ipairs({
    'onMissionLoadBegin', 'onMissionLoadProgress', 'onMissionLoadEnd',
    'onSimulationStart', 'onSimulationStop', 'onSimulationPause', 'onSimulationResume',
    'onNetConnect', 'onNetDisconnect', 'onNetMissionChanged', 'onNetMissionEnd',
    'onPlayerConnect', 'onPlayerDisconnect', 'onPlayerStart', 'onPlayerStop',
    'onPlayerChangeSlot', 'onGameEvent',
  }) do
    callbacks[name] = function(...)
      if stopped then
        return
      end
      if name == 'onMissionLoadBegin' then
        generation = generation + 1
        loading = true
      elseif name == 'onMissionLoadEnd' then
        loading = false
      end
      pcall(note, name, ...)
    end
  end

  callbacks.onSimulationFrame = function()
    if stopped then
      return
    end
    frames.total = frames.total + 1
    if loading then
      frames.loading = frames.loading + 1
    end
    if try('getPause') == true then
      frames.paused = frames.paused + 1
    end
    local ran, why = pcall(frame)
    if not ran then
      dcslog(log.ERROR, 'frame raised: ' .. tostring(why))
      if current then
        pcall(finish, current, nil, 'the hook raised: ' .. tostring(why))
      end
      current = nil
    end
  end

  DCS.setUserCallbacks(callbacks)
  dcslog(log.INFO, 'eval hook ready')
end)

if not ok then
  pcall(log.write, 'NANOPROBE-EVAL', log.ERROR, 'eval hook failed: ' .. tostring(err))
end
