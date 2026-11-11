-- The eval channel's prelude: what the eval hook installs in a Lua state
-- before it runs a snippet there. spec/eval.md specifies the channel.
--
-- Development only; never part of a release. It runs in every state the hook
-- can reach, so it uses the base library alone and reads every other global
-- under pcall. It installs one global, `__nanoprobe_eval`, and returns a
-- reply code. Every function the hook calls returns a string and never
-- raises: only a string is known to cross net.dostring_in and a_do_script.
--
-- Arguments: the state's name, and the development folder with a trailing
-- slash.

local STATE, DIR = ...
local NAME = '__nanoprobe_eval'
local G = _G

-- A global's value, or nil when it is absent or reading it raises.
local function global(name)
  local ok, value = pcall(function()
    return G[name]
  end)
  if ok then
    return value
  end
  return nil
end

if type(global(NAME)) == 'table' then
  return 'NPE:I'
end

local NP = { version = 1, state = STATE, dir = DIR, gen = 0 }

-- Text for any value, with no metamethod able to raise through it.
local function describe(value)
  if type(value) == 'string' then
    return value
  end
  local ok, text = pcall(tostring, value)
  return ok and type(text) == 'string' and text or ('<' .. type(value) .. '>')
end

local function pack(...)
  return { n = select('#', ...), ... }
end

---------------------------------------------------------------------------
-- JSON encoding, with the limits spec/eval.md gives.

local MAX_DEPTH = 8
local MAX_ENTRIES = 2000

local escapes = {
  ['"'] = '\\"',
  ['\\'] = '\\\\',
  ['\b'] = '\\b',
  ['\f'] = '\\f',
  ['\n'] = '\\n',
  ['\r'] = '\\r',
  ['\t'] = '\\t',
}

-- Whether `s` is well-formed UTF-8, checked by removing every well-formed
-- sequence and looking for a high byte that is left.
local function utf8(s)
  if not s:find('[\128-\255]') then
    return true
  end
  local rest = s:gsub('[\240-\244][\128-\191][\128-\191][\128-\191]', '')
  rest = rest:gsub('[\224-\239][\128-\191][\128-\191]', '')
  rest = rest:gsub('[\194-\223][\128-\191]', '')
  return not rest:find('[\128-\255]')
end

local function quote(s)
  s = s:gsub('[%c"\\]', function(c)
    return escapes[c] or string.format('\\u%04x', c:byte())
  end)
  if not utf8(s) then
    -- Not UTF-8: write each high byte as the code point of the same number.
    s = s:gsub('[\128-\255]', function(c)
      return string.format('\\u%04x', c:byte())
    end)
  end
  return '"' .. s .. '"'
end

local function number(n)
  if n ~= n then
    return '"<nan>"'
  elseif n == math.huge then
    return '"<inf>"'
  elseif n == -math.huge then
    return '"<-inf>"'
  end
  if n == math.floor(n) and math.abs(n) < 2 ^ 53 then
    return string.format('%d', n)
  end
  for digits = 15, 16 do
    local s = string.format('%.' .. digits .. 'g', n)
    if tonumber(s) == n then
      return s
    end
  end
  return string.format('%.17g', n)
end

local function encode(value, depth, seen)
  local t = type(value)
  if t == 'string' then
    return quote(value)
  elseif t == 'number' then
    return number(value)
  elseif t == 'boolean' then
    return tostring(value)
  elseif t == 'nil' then
    return 'null'
  elseif t ~= 'table' then
    return '"<' .. t .. '>"'
  end
  depth = depth or 1
  seen = seen or {}
  if seen[value] then
    return '"<cycle>"'
  end
  if depth > MAX_DEPTH then
    return '"<depth>"'
  end
  seen[value] = true
  local count = 0
  for _ in pairs(value) do
    count = count + 1
  end
  local out = {}
  local length = #value
  if count > 0 and count == length then
    for i = 1, math.min(length, MAX_ENTRIES) do
      out[i] = encode(value[i], depth + 1, seen)
    end
    if length > MAX_ENTRIES then
      out[#out + 1] = '"<more>"'
    end
    seen[value] = nil
    return '[' .. table.concat(out, ',') .. ']'
  end
  local keys, by_key = {}, {}
  for k, v in pairs(value) do
    local key = type(k) == 'string' and k or describe(k)
    if by_key[key] == nil then
      keys[#keys + 1] = key
    end
    by_key[key] = v
  end
  table.sort(keys)
  for i = 1, math.min(#keys, MAX_ENTRIES) do
    out[i] = quote(keys[i]) .. ':' .. encode(by_key[keys[i]], depth + 1, seen)
  end
  if #keys > MAX_ENTRIES then
    out[#out + 1] = '"<more>":true'
  end
  seen[value] = nil
  return '{' .. table.concat(out, ',') .. '}'
end

-- Never raises: a value that defeats the encoder becomes a string saying so.
local function json(value)
  local ok, text = pcall(encode, value)
  if ok then
    return text
  end
  return quote('<unencodable: ' .. describe(text) .. '>')
end

NP.json = json

---------------------------------------------------------------------------
-- Clocks and the timing helper.

-- The best clock this state has: DCS.getRealTime (about 100 ns, PRIOR), then
-- os.clock (1 ms, PRIOR), then none.
local function clock_source()
  local dcs = global('DCS')
  if type(dcs) == 'table' then
    local ok, f = pcall(function()
      return dcs.getRealTime
    end)
    if ok and type(f) == 'function' then
      return f, 'DCS.getRealTime'
    end
  end
  local os_ = global('os')
  if type(os_) == 'table' and type(os_.clock) == 'function' then
    return os_.clock, 'os.clock'
  end
  return nil, 'none'
end

local function clock()
  local f = clock_source()
  return f and f() or nil
end

-- Time `fn`: `n` calls, `rounds` times over, keeping the best round. `ns` is
-- the time per call with the cost of an empty call removed; `raw_ns` is not
-- adjusted.
local function bench(fn, n, rounds)
  n = n or 10000
  rounds = rounds or 5
  local read, name = clock_source()
  local result = { clock = name, n = n, rounds = rounds }
  if not read then
    return result
  end
  local function empty() end
  local best, base
  for _ = 1, rounds do
    local t0 = read()
    for _ = 1, n do
      empty()
    end
    local t1 = read()
    for _ = 1, n do
      fn()
    end
    local t2 = read()
    if not base or t1 - t0 < base then
      base = t1 - t0
    end
    if not best or t2 - t1 < best then
      best = t2 - t1
    end
  end
  result.raw_ns = best / n * 1e9
  result.ns = math.max(0, best - base) / n * 1e9
  result.total_s = best
  return result
end

---------------------------------------------------------------------------
-- The cleanup registry.

local registry = {}
NP.store = {}
NP.libs = {}
NP.pending = {}
NP.results = {}
NP.faults = {}

-- A failure in a timer or an event handler a snippet installed. The next
-- reply carries it.
local function fault(text)
  if #NP.faults < 20 then
    NP.faults[#NP.faults + 1] = text
  end
end

local function push(entry)
  registry[#registry + 1] = entry
  return entry
end

local track = {}

function track.defer(label, fn)
  push({ kind = 'deferred', label = label, undo = fn })
end

function track.global(name, value)
  G[name] = value
  push({
    kind = 'global',
    label = name,
    undo = function()
      G[name] = nil
    end,
  })
end

function track.timer(fn, arg, time)
  local timer = global('timer')
  if type(timer) ~= 'table' then
    error('E.track.timer: this state has no timer', 2)
  end
  local entry = { kind = 'timer', gen = NP.gen }
  local id = timer.scheduleFunction(function(a, t)
    if entry.dead then
      return nil
    end
    local ok, next_time = pcall(fn, a, t)
    if ok and type(next_time) == 'number' then
      return next_time
    end
    entry.done = true
    if not ok then
      fault('timer: ' .. describe(next_time))
    end
    return nil
  end, arg, time)
  entry.undo = function()
    entry.dead = true
    -- A mission load clears timers (LIVE), and the ID may be another
    -- timer's since, so only a timer of this mission is removed.
    -- Removing a timer that has run raises (LIVE), so one that finished is
    -- left alone.
    if entry.gen == NP.gen and not entry.done then
      pcall(timer.removeFunction, id)
    end
  end
  push(entry)
  return id
end

function track.handler(handler)
  local world = global('world')
  if type(world) ~= 'table' then
    error('E.track.handler: this state has no world', 2)
  end
  local entry = { kind = 'handler' }
  local wrapper = {}
  -- ED's dispatch has no pcall (ED-CODE, Scripts/World/EventHandlers.lua),
  -- so this function must not raise.
  function wrapper:onEvent(event)
    if entry.dead then
      return
    end
    local ok, err = pcall(handler.onEvent, handler, event)
    if not ok then
      fault('handler: ' .. describe(err))
    end
  end
  world.addEventHandler(wrapper)
  entry.undo = function()
    entry.dead = true
    world.removeEventHandler(wrapper)
  end
  push(entry)
  return wrapper
end

-- What exists at install, to compare against at reset.
local function global_names()
  local names = {}
  for k in pairs(G) do
    if type(k) == 'string' then
      names[k] = true
    end
  end
  return names
end

local function handler_count()
  local world = global('world')
  if type(world) ~= 'table' then
    return nil
  end
  local ok, handlers = pcall(function()
    return world.eventHandlers
  end)
  if not ok or type(handlers) ~= 'table' then
    return nil
  end
  local n = 0
  for _ in pairs(handlers) do
    n = n + 1
  end
  return n
end

local baseline = { globals = global_names(), handlers = handler_count() }

-- Run every cleanup, last in first out, and report.
local function reset()
  local cleaned, errors = {}, {}
  for i = #registry, 1, -1 do
    local entry = registry[i]
    registry[i] = nil
    local ok, err = pcall(entry.undo)
    if ok then
      cleaned[entry.kind] = (cleaned[entry.kind] or 0) + 1
    else
      errors[#errors + 1] = entry.kind .. ' ' .. (entry.label or '') .. ': ' .. describe(err)
    end
  end
  NP.store, NP.libs, NP.pending, NP.results = {}, {}, {}, {}
  for _, text in ipairs(NP.faults) do
    errors[#errors + 1] = text
  end
  NP.faults = {}

  local new_globals = {}
  for name in pairs(global_names()) do
    if not baseline.globals[name] and name ~= NAME and #new_globals < 50 then
      new_globals[#new_globals + 1] = name
    end
  end
  table.sort(new_globals)
  local now = handler_count()
  local parts = {
    '"state":' .. quote(STATE),
    '"cleaned":' .. json(cleaned),
    '"errors":[' .. table.concat((function()
      local out = {}
      for i, text in ipairs(errors) do
        out[i] = quote(text)
      end
      return out
    end)(), ',') .. ']',
    '"new_globals":[' .. table.concat((function()
      local out = {}
      for i, name in ipairs(new_globals) do
        out[i] = quote(name)
      end
      return out
    end)(), ',') .. ']',
    '"handlers":' .. ((now and baseline.handlers) and number(now - baseline.handlers) or 'null'),
  }
  return '{' .. table.concat(parts, ',') .. '}'
end

---------------------------------------------------------------------------
-- Observations.

local function equal(a, b, within, depth)
  if type(a) ~= type(b) then
    return false
  end
  if type(a) == 'number' and within then
    return math.abs(a - b) <= within
  end
  if type(a) ~= 'table' then
    return a == b
  end
  depth = depth or 1
  if depth > MAX_DEPTH then
    return false
  end
  for k, v in pairs(a) do
    if not equal(v, b[k], within, depth + 1) then
      return false
    end
  end
  for k in pairs(b) do
    if a[k] == nil then
      return false
    end
  end
  return true
end

-- `match`, `differ`, or nil when `expect` holds nothing for `platform`. The
-- mock is built from a listing taken under Wine, so it falls back to the Wine
-- expectation and then the Windows one. A timing matches within a factor (10
-- unless `expect.factor` says otherwise); under the mock a timing only has to
-- be a finite number that is not negative.
local function judge(value, expect, kind, platform)
  local expected = expect[platform]
  if expected == nil and platform == 'mock' then
    expected = expect.wine
    if expected == nil then
      expected = expect.windows
    end
  end
  if expected == nil then
    return nil
  end
  if kind == 'timing' then
    if type(value) ~= 'number' or value ~= value or value < 0 or value == math.huge then
      return 'differ'
    end
    if platform == 'mock' then
      return 'match'
    end
    if value == 0 or expected == 0 then
      return value == expected and 'match' or 'differ'
    end
    local factor = expect.factor or 10
    local ratio = value / expected
    return (ratio <= factor and ratio >= 1 / factor) and 'match' or 'differ'
  end
  return equal(value, expected, expect.within) and 'match' or 'differ'
end

NP.judge = judge

local PLATFORMS = { 'windows', 'wine', 'mock' }

local function observation(o)
  local parts = { '"name":' .. quote(o.name), '"value":' .. json(o.value), '"kind":' .. quote(o.kind) }
  if o.expect then
    local expect, verdict = {}, {}
    for _, platform in ipairs(PLATFORMS) do
      if o.expect[platform] ~= nil then
        expect[#expect + 1] = quote(platform) .. ':' .. json(o.expect[platform])
      end
      local v = judge(o.value, o.expect, o.kind, platform)
      if v then
        verdict[#verdict + 1] = quote(platform) .. ':' .. quote(v)
      end
    end
    parts[#parts + 1] = '"expect":{' .. table.concat(expect, ',') .. '}'
    parts[#parts + 1] = '"verdict":{' .. table.concat(verdict, ',') .. '}'
    if o.expect.within then
      parts[#parts + 1] = '"within":' .. number(o.expect.within)
    end
  end
  return '{' .. table.concat(parts, ',') .. '}'
end

---------------------------------------------------------------------------
-- Running a snippet.

local traceback = type(global('debug')) == 'table' and global('debug').traceback or nil

local function handler(err)
  local text = describe(err)
  if type(err) ~= 'string' then
    text = 'error value of type ' .. type(err) .. ': ' .. text
  end
  if traceback then
    local ok, trace = pcall(traceback, text, 2)
    if ok and type(trace) == 'string' then
      return trace
    end
  end
  return text
end

local WAIT = {}

-- What a snippet and a library share, and what lasts between runs.
local api = {
  state = STATE,
  track = track,
  bench = bench,
  clock = clock,
  json = json,
  pack = pack,
  global = global,
  describe = describe,
}
NP.api = api

function api.lib(name)
  if NP.libs[name] then
    return NP.libs[name]
  end
  if type(name) ~= 'string' or not name:match('^[%w_%-]+$') then
    error('E.lib: bad library name', 2)
  end
  local io_ = global('io')
  if type(io_) ~= 'table' then
    error('E.lib: this state has no io to read a library with', 2)
  end
  local path = DIR .. 'lua/' .. name .. '.lua'
  local file, err = io_.open(path, 'rb')
  if not file then
    error('E.lib: ' .. describe(err), 2)
  end
  local source = file:read('*a')
  file:close()
  local chunk, cerr = loadstring(source, '=' .. name .. '.lua')
  if not chunk then
    error(cerr, 0)
  end
  setfenv(chunk, setmetatable({}, { __index = G }))
  local lib = chunk(api)
  NP.libs[name] = lib
  return lib
end

-- `running` asks the hook to resume the simulation whenever it finds it
-- paused, for a snippet that waits on model time.
function api.wait(fn, seconds, running)
  return { [WAIT] = true, fn = fn, seconds = seconds or 10, running = running == 'running' }
end

-- The reply for a finished run, stored for the hook to take in pieces.
local function finish(ctx, ok, result)
  local parts = { '"ok":' .. tostring(ok) }
  if ok then
    local values = {}
    for i = 1, result.n do
      values[i] = json(result[i])
    end
    parts[#parts + 1] = '"n":' .. number(result.n)
    parts[#parts + 1] = '"values":[' .. table.concat(values, ',') .. ']'
  else
    parts[#parts + 1] = '"error":' .. quote(describe(result))
    parts[#parts + 1] = '"n":0'
    parts[#parts + 1] = '"values":[]'
  end
  local observations = {}
  for i, o in ipairs(ctx.observations) do
    observations[i] = observation(o)
  end
  parts[#parts + 1] = '"observations":[' .. table.concat(observations, ',') .. ']'
  for _, text in ipairs(NP.faults) do
    ctx.notes[#ctx.notes + 1] = 'fault: ' .. text
  end
  NP.faults = {}
  local notes = {}
  for i, text in ipairs(ctx.notes) do
    notes[i] = quote(text)
  end
  parts[#parts + 1] = '"notes":[' .. table.concat(notes, ',') .. ']'
  local text = '{' .. table.concat(parts, ',') .. '}'
  NP.pending[ctx.id] = nil
  NP.results[ctx.id] = text
  return 'NPE:D:' .. #text
end

local function context(id)
  local ctx = { id = id, observations = {}, notes = {} }
  local E = setmetatable({ id = id, store = NP.store }, { __index = api })
  function E.observe(name, value, expect)
    local kind = 'exact'
    if expect and expect.timing then
      kind = 'timing'
    elseif expect and expect.info then
      kind = 'info'
    end
    ctx.observations[#ctx.observations + 1] = {
      name = tostring(name),
      value = value,
      kind = kind,
      expect = expect,
    }
    return value
  end
  function E.note(text)
    ctx.notes[#ctx.notes + 1] = describe(text)
  end
  function E.cross(state, source)
    if not NP.cross then
      error('E.cross works only in the hook state', 2)
    end
    return NP.cross(state, source)
  end
  E.hook = NP.hook
  ctx.E = E
  return ctx
end

local function compile(ctx, source)
  local chunk, err = loadstring(source, '=' .. ctx.id)
  if not chunk then
    return nil, err
  end
  local env = setmetatable({ E = ctx.E }, { __index = G })
  setfenv(chunk, env)
  return chunk
end

-- Settle a run or a poll: finished, failed, or still waiting.
local function settle(ctx, results, now)
  if not results[1] then
    return finish(ctx, false, results[2])
  end
  local first = results[2]
  if type(first) == 'table' and first[WAIT] then
    NP.pending[ctx.id] = {
      ctx = ctx,
      fn = first.fn,
      deadline = now + first.seconds,
      seconds = first.seconds,
      running = first.running,
    }
    return first.running and 'NPE:R' or 'NPE:W'
  end
  local values = { n = results.n - 1 }
  for i = 2, results.n do
    values[i - 1] = results[i]
  end
  return finish(ctx, true, values)
end

-- Run `source` as request `id`. `gen` is the mission generation and `now`
-- the hook's real time, which is the only clock every state can be given.
function NP.run(id, source, gen, now)
  local ok, reply = pcall(function()
    NP.gen = gen
    local ctx = context(id)
    local chunk, err = compile(ctx, source)
    if not chunk then
      return finish(ctx, false, 'compile: ' .. describe(err))
    end
    return settle(ctx, pack(xpcall(function()
      return chunk(ctx.E)
    end, handler)), now)
  end)
  return ok and reply or ('NPE:X:' .. describe(reply))
end

-- Call a waiting request's function again.
function NP.poll(id, gen, now)
  local ok, reply = pcall(function()
    NP.gen = gen
    local pending = NP.pending[id]
    if not pending then
      return 'NPE:X:no request ' .. id .. ' is waiting'
    end
    local results = pack(xpcall(pending.fn, handler))
    if not results[1] then
      return finish(pending.ctx, false, results[2])
    end
    if results.n > 1 and results[2] ~= nil then
      local values = { n = results.n - 1 }
      for i = 2, results.n do
        values[i - 1] = results[i]
      end
      return finish(pending.ctx, true, values)
    end
    if now > pending.deadline then
      return finish(pending.ctx, false, 'E.wait timed out after ' .. pending.seconds .. ' s')
    end
    return pending.running and 'NPE:R' or 'NPE:W'
  end)
  return ok and reply or ('NPE:X:' .. describe(reply))
end

-- Run `source` now and reply with its first value as text. For E.cross.
function NP.plain(id, source, gen)
  local ok, reply = pcall(function()
    NP.gen = gen
    local ctx = context(id)
    local chunk, err = compile(ctx, source)
    if not chunk then
      return 'NPE:X:compile: ' .. describe(err)
    end
    local results = pack(xpcall(function()
      return chunk(ctx.E)
    end, handler))
    if not results[1] then
      return 'NPE:X:' .. describe(results[2])
    end
    local first = results[2]
    if type(first) == 'table' and first[WAIT] then
      return 'NPE:X:a crossed snippet cannot wait'
    end
    if type(first) == 'number' then
      return 'NPE:V:' .. (first == math.floor(first) and string.format('%d', first) or string.format('%.17g', first))
    end
    return 'NPE:V:' .. describe(first)
  end)
  return ok and reply or ('NPE:X:' .. describe(reply))
end

-- A piece of a stored reply, bytes `from` to `to`.
function NP.take(id, from, to)
  local text = NP.results[id]
  if type(text) ~= 'string' then
    return ''
  end
  return text:sub(from, to)
end

function NP.drop(id)
  NP.results[id] = nil
  return 'NPE:I'
end

-- Run every cleanup and store the report as request `id`'s reply.
function NP.reset(id)
  local ok, text = pcall(reset)
  if not ok then
    text = '{"state":' .. quote(STATE) .. ',"cleaned":{},"errors":[' .. quote(describe(text))
      .. '],"new_globals":[],"handlers":null}'
  end
  NP.results[id] = text
  return 'NPE:D:' .. #text
end

function NP.uninstall()
  G[NAME] = nil
  return 'NPE:U'
end

-- Run `chunk` in the sandboxed mission scripting environment. Only the
-- mission state has a_do_script, and only with a mission loaded (PRIOR).
-- a_do_script hands back what the chunk returned shifted by one, so a lone
-- value is lost (LIVE for three values; PRIOR for one). The chunk therefore
-- returns its reply and a spare value, and the reply arrives second. Only a
-- string may cross: a table returned through a_do_script corrupted DCS's
-- allocator (PRIOR).
function NP.sse(chunk)
  local ok, reply = pcall(function()
    local a_do_script = global('a_do_script')
    if type(a_do_script) ~= 'function' then
      return 'NPE:N'
    end
    local results = pack(a_do_script('return (function() ' .. chunk .. '\nend)(), 0'))
    for i = 1, results.n do
      if type(results[i]) == 'string' then
        return results[i]
      end
    end
    local types = {}
    for i = 1, results.n do
      types[i] = type(results[i])
    end
    return 'NPE:X:a_do_script returned no string: ' .. table.concat(types, ', ')
  end)
  return ok and reply or ('NPE:X:' .. describe(reply))
end

G[NAME] = NP
return 'NPE:I'
