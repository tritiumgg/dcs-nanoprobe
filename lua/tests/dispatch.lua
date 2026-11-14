-- The agent's dispatch rules for a command with a `server` handler
-- (spec/commands.md, "Handlers"), on the host: the hook in the strict mock, the
-- host-native agent, and `link-probe` as the sidecar over a real mapped file
-- and a real loopback handshake.
--
-- `link-probe` writes one command after each HELLO that reaches the agent as a
-- `server` command that passes admission: MESSAGE, seq 1, for the active
-- campaign. The tests vary the shim the hook loads and the lifecycle state:
--
-- * no shim once the mission runs: the command is answered NOT_READY;
-- * a shim whose pump takes the command and never calls `command_done`: the
--   agent answers LUA_ERROR; and a `command_done` with nothing outstanding,
--   called before and after, changes nothing.
--
-- The scenarios that need other commands use `command-script` as the sidecar:
-- it sends the commands a JSON file lists and prints one line per RESULT.
--
-- * budget: a handler slower than the commanded-work budget leaves the next
--   command in the ring, and it is answered APPLIED on a later frame;
-- * raw-off: EXTENSION_SET with raw Lua off and no shim is RAW_LUA_OFF;
-- * unknown-extension: EXTENSION_INVOKE of a name nobody registered is
--   LUA_ERROR;
-- * unknown-raw-off, unknown-no-shim: the same invoke is RAW_LUA_OFF, then
--   NOT_READY;
-- * begin-only-pump: a forced pump that calls pump_begin and never reaches
--   command() leaves the head command NOT_READY;
-- * bounded-results: a 1 MiB error message, a 10,000-key return, strings over
--   the read limit, and a value over the record limit;
-- * bad-code: a handler code of 65536 is LUA_ERROR;
-- * deep-tables: tables 16 deep and a cyclic one, as an argument and as an
--   extension's return value, are answered and do not crash; a 17-level
--   argument sent as raw bytes is BAD_PAYLOAD, and the next command is answered.
--
-- The agent keeps its state for the life of the process, so each scenario is a
-- process of its own: the last argument names it.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE, _, _, ONLY = ...
assert(MODULE and PROBE and ONLY,
  'usage: lua lua/tests/dispatch.lua <module> <version> <link-probe> <nanoprobe> <xtask> <scenario>')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local COMMAND_SCRIPT = PROBE:gsub('link%-probe$', 'command-script')
local APPLIED, NOT_READY, LUA_ERROR, RAW_LUA_OFF, TOO_LARGE = 0, 27, 23, 21, 25
local BAD_PAYLOAD = 19

local created = {}
local ran = false

-- A shim whose handler for every command takes longer than the commanded-work
-- budget (1 ms in `agent.toml`), so that the pump gets no second command. Each
-- pump records how many commands it took.
local SLOW_SHIM = [[
local agent = ...
local M = { pumps = {} }
function M.start()
  return 'slow start'
end
function M.pump()
  if agent.pump_begin(0, 0, 0) == nil then
    return
  end
  local taken = 0
  while agent.command() do
    taken = taken + 1
    local x = 0
    for i = 1, 3000000 do
      x = x + i
    end
    agent.command_done(0)
  end
  M.pumps[#M.pumps + 1] = taken
  agent.pump_end()
end
return M
]]

-- A string as JSON.
local function q(text)
  return '"' .. text:gsub('[\\"]', '\\%0'):gsub('\n', '\\n') .. '"'
end

-- A shim that takes the command and does not answer it. `command_done` with
-- nothing outstanding runs before the command is taken and after the pump
-- ends, when the agent has answered the command itself.
local NO_ANSWER_SHIM = [[
local agent = ...
local M = { starts = 0 }
function M.start()
  M.starts = M.starts + 1
  return 'stub start'
end
function M.pump()
  agent.pump_begin(0, 0, 0)
  agent.command_done(0, 'stray before')
  local opcode = agent.command()
  M.took = opcode
  agent.pump_end()
  agent.command_done(0, 'stray after')
end
return M
]]

-- One run: a profile with `shim` (a source text, or nil for none), the probe,
-- and frames until `done`.
local function scenario(name, shim, body, extra_toml)
  if name ~= ONLY then
    return
  end
  ran = true
  local profile = S.scratch()
  created[#created + 1] = profile
  local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
  local map = profile .. 'nanoprobe/link.map'
  local out = profile .. 'probe.jsonl'
  local pidfile = profile .. 'probe.pid'
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  if shim then
    S.write(version .. 'lua/shim.lua', shim)
  end
  S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
  local port = 20000 + math.floor(os.clock() * 1e6 + os.time()) % 20000
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 256\nin_ring_kib = 256\n" .. (extra_toml or ''), map, port))
  S.write(profile .. 'nanoprobe/token', TOKEN)

  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')

  local ctx = {}
  function ctx.events()
    local list = {}
    for _, line in ipairs(S.lines(out)) do
      local ok, e = pcall(S.json, line)
      if ok and type(e) == 'table' then
        list[#list + 1] = e
      end
    end
    return list
  end
  function ctx.result(seq)
    for _, e in ipairs(ctx.events()) do
      if e.ev == 'result' and e.seq == seq then
        return e
      end
    end
  end
  -- `step` is the frame to run: host:frame, or while a mission loads, where the
  -- mock runs none, the hook's own frame callback.
  function ctx.run(seconds, done, step)
    step = step or function()
      host:frame(0.01)
    end
    local deadline = now() + seconds
    local next_frame = now()
    while now() < deadline do
      if now() >= next_frame then
        next_frame = next_frame + 0.01
        step()
        if done and done() then
          return true
        end
      end
    end
    return done == nil
  end
  function ctx.attached()
    for _, e in ipairs(ctx.events()) do
      if e.ev == 'hello' then
        return true
      end
    end
    return false
  end
  ctx.host = host
  ctx.out = out
  ctx.probe = function()
    assert(os.execute(string.format(
      '%s --map %s --port %d --token-file %s --seconds 60 > %s 2>&1 & echo $! > %s',
      S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(out),
      S.quote(pidfile))) == 0, 'could not start link-probe')
  end
  -- `command-script` as the sidecar, for the script `text` (JSON).
  ctx.script = function(text)
    S.write(profile .. 'script.json', text)
    assert(os.execute(string.format(
      '%s --map %s --port %d --token-file %s --script %s --seconds 60 > %s 2>&1 & echo $! > %s',
      S.quote(COMMAND_SCRIPT), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'),
      S.quote(profile .. 'script.json'), S.quote(out), S.quote(pidfile))) == 0,
      'could not start command-script')
  end
  -- Whether the script's `end` line is in.
  function ctx.ended()
    for _, e in ipairs(ctx.events()) do
      if e.ev == 'end' then
        return e
      end
    end
  end
  ctx.stop = function()
    local pid = (S.read(pidfile) or ''):match('%d+')
    if pid then
      os.execute('kill ' .. pid .. ' 2>/dev/null')
    end
  end
  local ok, err = pcall(body, ctx)
  ctx.stop()
  if not ok then
    io.stderr:write(S.read(out) or '', '\n')
  end
  if not ok then
    for _, f in ipairs({ 'nanoprobe/logs/hook.log', 'nanoprobe/logs/agent.log' }) do
      io.stderr:write(f, '\n', S.read(profile .. f) or 'none', '\n')
    end
  end
  assert(ok, err)
end

local function dump(ctx)
  return S.read(ctx.out) or ''
end

-- 1. No shim once the mission runs: NOT_READY. The wait in LOADING cannot be
-- driven: the mock runs no frame while a mission loads, and a frame the hook
-- calls itself then reads the state LOADING as RUNNING before commands run
-- (`frame_with`), so the LOADING branch in `step` has no caller on a frame.
scenario('no-shim', nil, function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.probe()
  assert(ctx.run(10, function()
    return ctx.result(1) ~= nil
  end), 'no answer once the mission ran, no shim:\n' .. dump(ctx))
  assert(ctx.result(1).code == NOT_READY,
    'no shim outside LOADING answered code ' .. tostring(ctx.result(1).code) .. ', not NOT_READY:\n' .. dump(ctx))
  S.ok('no shim: a server command is answered NOT_READY once the mission runs')
end)

-- 2. A command taken and not answered is LUA_ERROR; a stray command_done is
-- ignored.
scenario('no-answer', NO_ANSWER_SHIM, function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.probe()
  assert(ctx.run(10, function()
    return ctx.result(1) ~= nil
  end), 'no answer for a command the shim took and left:\n' .. dump(ctx))
  assert(ctx.result(1).code == LUA_ERROR,
    'a command taken and not done answered code ' .. tostring(ctx.result(1).code) .. ', not LUA_ERROR:\n' .. dump(ctx))
  ctx.run(0.5)
  local results = 0
  for _, e in ipairs(ctx.events()) do
    if e.ev == 'result' and e.seq == 1 then
      results = results + 1
    end
  end
  assert(results == 1, 'the command was answered ' .. results .. ' times:\n' .. dump(ctx))
  S.ok('a command taken and not done is answered LUA_ERROR once, and a stray command_done changes nothing')
end)

-- The results of the script's run, by seq, once its `end` line is in.
local function finish(ctx, what)
  assert(ctx.run(30, function()
    return ctx.ended() ~= nil
  end), what .. ': no end line:\n' .. dump(ctx))
  local ended = ctx.ended()
  assert(ended.complete, what .. ': a command got no RESULT:\n' .. dump(ctx))
  assert(ended.duplicates == 0,
    what .. ': ' .. tostring(ended.duplicates) .. ' duplicate RESULTs:\n' .. dump(ctx))
end

-- 3. A handler slower than the budget leaves the next command in the ring: it
-- is answered APPLIED on a later frame, not NOT_READY.
scenario('budget', SLOW_SHIM, function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.script('{"commands":[{"op":"MESSAGE"},{"op":"MESSAGE"}]}')
  finish(ctx, 'two commands, one over the budget')
  for seq = 1, 2 do
    local r = ctx.result(seq)
    assert(r and r.code == APPLIED,
      'command ' .. seq .. ' answered ' .. tostring(r and r.code_name) .. ', not APPLIED:\n' .. dump(ctx))
  end
  local taken = {}
  for _, n in ipairs(ctx.host.states.server.__nanoprobe_shim.pumps) do
    if n > 0 then
      taken[#taken + 1] = n
    end
  end
  assert(#taken == 2 and taken[1] == 1 and taken[2] == 1,
    'the pumps that took commands took ' .. table.concat(taken, ',') .. ', not 1 then 1:\n' .. dump(ctx))
  S.ok('a handler over the budget leaves the next command in the ring, answered APPLIED on a later frame')
end, '\n[budget]\ncommanded_ms = 1.0\n')

-- 4. EXTENSION_SET with raw Lua off and no shim: RAW_LUA_OFF, not NOT_READY.
scenario('raw-off', nil, function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.script('{"campaign":7,"commands":[{"op":"EXTENSION_SET","name":"x","rev":1,'
    .. '"source":"return {}"}]}')
  finish(ctx, 'EXTENSION_SET, raw Lua off')
  local r = ctx.result(1)
  assert(r and r.code == RAW_LUA_OFF,
    'EXTENSION_SET with raw Lua off answered ' .. tostring(r and r.code_name) .. ', not RAW_LUA_OFF:\n' .. dump(ctx))
  S.ok('EXTENSION_SET with raw Lua off and no shim is answered RAW_LUA_OFF')
end)

-- 5. EXTENSION_INVOKE of a name nobody registered: LUA_ERROR.
scenario('unknown-extension', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.script('{"campaign":7,"commands":[{"op":"EXTENSION_INVOKE","name":"nobody","args":[1]}]}')
  finish(ctx, 'EXTENSION_INVOKE, no extension')
  local r = ctx.result(1)
  assert(r and r.code == LUA_ERROR,
    'EXTENSION_INVOKE of an unknown name answered ' .. tostring(r and r.code_name) .. ', not LUA_ERROR:\n' .. dump(ctx))
  assert(r.message == 'no extension of that name',
    'EXTENSION_INVOKE of an unknown name: message ' .. tostring(r.message) .. ':\n' .. dump(ctx))
  S.ok('EXTENSION_INVOKE of a name with no extension is answered LUA_ERROR, "no extension of that name"')
end, '\n[lua]\nraw = true\n')

local INVOKE_UNKNOWN = '{"campaign":7,"commands":[{"op":"EXTENSION_INVOKE","name":"nobody","args":[1]}]}'

-- 5b. The same invoke with raw Lua off is RAW_LUA_OFF, and with raw Lua on and
-- no shim NOT_READY.
scenario('unknown-raw-off', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.script(INVOKE_UNKNOWN)
  finish(ctx, 'EXTENSION_INVOKE, raw Lua off')
  assert(ctx.result(1).code == RAW_LUA_OFF,
    'EXTENSION_INVOKE of an unknown name, raw off: ' .. tostring(ctx.result(1).code_name) .. ':\n' .. dump(ctx))
  S.ok('EXTENSION_INVOKE of an unknown name with raw Lua off is RAW_LUA_OFF')
end)

scenario('unknown-no-shim', nil, function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.script(INVOKE_UNKNOWN)
  finish(ctx, 'EXTENSION_INVOKE, no shim')
  assert(ctx.result(1).code == NOT_READY,
    'EXTENSION_INVOKE of an unknown name, no shim: ' .. tostring(ctx.result(1).code_name) .. ':\n' .. dump(ctx))
  S.ok('EXTENSION_INVOKE of an unknown name with no shim is NOT_READY')
end, '\n[lua]\nraw = true\n')

-- 6. Tables 16 deep and a cyclic one, as an argument and as a return value.
local EXTENSION = [[
return { invoke = function(mode, n)
  if mode == 'deep' then
    local t = {}
    local c = t
    for i = 2, n do
      c[1] = {}
      c = c[1]
    end
    return t
  elseif mode == 'cycle' then
    local t = {}
    t.me = t
    return t
  elseif mode == 'depth' then
    local d = 0
    while type(n) == 'table' do
      d = d + 1
      n = n[1]
    end
    return d
  end
end }
]]
-- 17 nested tables as raw lv bytes: a table of one pair, key 1, per level, and
-- nil at the bottom. The script's writer cannot write it, and the codec refuses it.
local DEEP17 = string.rep('0501000000' .. '03' .. '000000000000f03f', 17) .. '00'
scenario('deep-tables', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  ctx.host:load_mission('first', { paused = true })
  local nest = '1'
  for _ = 1, 16 do
    nest = '[' .. nest .. ']'
  end
  local function invoke(args)
    return '{"op":"EXTENSION_INVOKE","name":"deep","args":' .. args .. ',"wait_result":true}'
  end
  ctx.script('{"campaign":7,"commands":[{"op":"EXTENSION_SET","name":"deep","rev":1,"source":'
    .. q(EXTENSION) .. ',"wait_result":true},'
    .. invoke('["depth",' .. nest .. ']') .. ','
    .. invoke('["deep",16]') .. ','
    .. invoke('["deep",17]') .. ','
    .. invoke('["cycle"]') .. ','
    .. '{"op":"EXTENSION_INVOKE","name":"deep","args_hex":"' .. DEEP17 .. '","wait_result":true},'
    .. invoke('["depth",1]') .. ']}')
  finish(ctx, 'deep and cyclic tables')
  local names = { 'EXTENSION_SET', 'a 16-level argument', 'a 16-deep return', 'a 17-deep return',
    'a cyclic return' }
  for seq, what in ipairs(names) do
    local r = ctx.result(seq)
    assert(r, what .. ' got no RESULT:\n' .. dump(ctx))
    assert(r.code == APPLIED, what .. ' answered ' .. tostring(r.code_name) .. ', not APPLIED:\n' .. dump(ctx))
  end
  -- An argument 17 tables deep, sent as raw bytes, fails the codec in the
  -- agent: BAD_PAYLOAD, and the command after it is answered.
  local bad = ctx.result(6)
  assert(bad and bad.code == BAD_PAYLOAD, 'a 17-level raw argument answered '
    .. tostring(bad and bad.code_name) .. ', not BAD_PAYLOAD:\n' .. dump(ctx))
  -- The values. The agent hands a handler `{name = .., args = ..}`, and `args`
  -- is the argument list, the argument's root. The codec admits 16 tables from
  -- that root: the list and 15 below it, which `invoke(mode, n)` sees as the
  -- 15 levels of `n`. The wrapper takes none of them. A lost lua_checkstack
  -- or depth guard changes this number.
  local seen = ctx.result(2).value
  assert(seen == 15, 'a 16-level argument: the handler saw ' .. tostring(seen) .. ' levels, not 15:\n' .. dump(ctx))
  assert(ctx.result(3).depth == 16 and ctx.result(3).value ~= nil,
    'a 16-deep return arrived ' .. tostring(ctx.result(3).depth) .. ' deep, not 16:\n' .. dump(ctx))
  -- Cut at 16 tables: the 17th is not read, and the 16th holds what is below as
  -- its type name or nothing, never a 17th table.
  assert(ctx.result(4).depth == 16,
    'a 17-deep return arrived ' .. tostring(ctx.result(4).depth) .. ' deep, not cut at 16:\n' .. dump(ctx))
  local bottom = ctx.result(4).value
  for _ = 1, 16 do
    bottom = bottom[1]
  end
  assert(bottom == '<table>', 'the cut of a 17-deep return holds ' .. tostring(bottom) .. ', not its type name:\n' .. dump(ctx))
  -- A cycle is cut where it meets itself: a table whose `me` is the type name.
  local c = ctx.result(5)
  assert(c.depth == 1 and type(c.value) == 'table' and c.value.me == '<table>',
    'the cyclic return came back as depth ' .. tostring(c.depth) .. ', me=' .. tostring(c.value and c.value.me)
      .. ':\n' .. dump(ctx))
  assert(ctx.result(7).value == 0, 'the command after the last answered ' .. tostring(ctx.result(7).value) .. ':\n' .. dump(ctx))
  S.ok('16 levels survive an argument and a return, 17 are cut at 16, a cycle is bounded, a 17-level raw argument is BAD_PAYLOAD, and the agent still answers')
end, '\n[lua]\nraw = true\n')

-- 7. A forced pump that never reached command(): the head server command is
-- answered NOT_READY by finish, once, and no pump is forced for it again. A
-- pump that skips `pump_begin` too is not covered: its sampling frames never
-- close, so no frame is forced and the command is never taken.
local BEGIN_ONLY_SHIM = [[
local agent = ...
local M = { pumps = 0, forced = 0 }
function M.start()
  return 'begin-only start'
end
function M.pump()
  M.pumps = M.pumps + 1
  local gen, sample = agent.pump_begin(0, 0, 0)
  if gen ~= nil and not sample then
    -- A pump the agent asked for only to read commands.
    M.forced = M.forced + 1
  end
  if gen ~= nil and sample then
    -- A sampling pump with nothing to read, as the shim's own: the gate closes
    -- and later frames are forced for the waiting command.
    agent.checks({})
    while agent.plan({}, {}) do
    end
  end
  agent.pump_end()
end
return M
]]
local function never_reached(shim)
  return function(ctx)
    ctx.host:load_mission('first', { paused = true })
    ctx.script('{"commands":[{"op":"MESSAGE"}]}')
    finish(ctx, 'a pump that never called command()')
    local r = ctx.result(1)
    assert(r and r.code == NOT_READY,
      'a pump that never reached command() answered ' .. tostring(r and r.code_name) .. ', not NOT_READY:\n' .. dump(ctx))
    local shim = ctx.host.states.server.__nanoprobe_shim
    local pumps, forced = shim.pumps, shim.forced
    assert(forced >= 1, 'no pump was forced for the command: ' .. forced)
    ctx.run(0.5)
    local results = 0
    for _, e in ipairs(ctx.events()) do
      if e.ev == 'result' then
        results = results + 1
      end
    end
    assert(results == 1, 'the command was answered ' .. results .. ' times:\n' .. dump(ctx))
    assert(shim.forced == forced,
      'the agent kept forcing pumps after the answer: ' .. forced .. ' then ' .. shim.forced
        .. ' (' .. pumps .. ' then ' .. shim.pumps .. ' pumps):\n' .. dump(ctx))
  end
end
scenario('begin-only-pump', BEGIN_ONLY_SHIM, function(ctx)
  never_reached(BEGIN_ONLY_SHIM)(ctx)
  S.ok('a forced pump that calls pump_begin and no command() leaves the head command NOT_READY, once')
end)

-- 8. Results the agent bounds: a 1 MiB error message, a return of 10,000 keys,
-- and a handler code that no `result` defines.
local BIG_SHIM = [[
return { invoke = function(mode, n)
  if mode == 'message' then
    error(string.rep('x', 1048576), 0)
  elseif mode == 'string' then
    return string.rep('x', n)
  elseif mode == 'list' then
    return { string.rep('x', n) }
  elseif mode == 'numbers' then
    local t = {}
    for i = 1, n do
      t[i] = i + 0.5
    end
    return t
  elseif mode == 'keys' then
    local t = {}
    for i = 1, 10000 do
      t['k' .. i] = i
    end
    return t
  end
end }
]]
scenario('bounded-results', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  ctx.host:load_mission('first', { paused = true })
  local function invoke(mode, n)
    return '{"op":"EXTENSION_INVOKE","name":"big","args":["' .. mode .. '",' .. (n or 0) .. '],"wait_result":true}'
  end
  ctx.script('{"campaign":7,"commands":[{"op":"EXTENSION_SET","name":"big","rev":1,"source":'
    .. q(BIG_SHIM) .. ',"wait_result":true},' .. invoke('message') .. ',' .. invoke('keys') .. ','
    .. invoke('string', 100000) .. ',' .. invoke('string', 20480) .. ',' .. invoke('list', 20000) .. ','
    .. invoke('numbers', 4000) .. ']}')
  finish(ctx, 'a huge message and a huge value')
  local m = ctx.result(2)
  assert(m.code == LUA_ERROR, 'a 1 MiB error answered ' .. tostring(m.code_name) .. ', not LUA_ERROR:\n' .. dump(ctx))
  assert(#m.message <= 1024 and #m.message > 0,
    'the error message is ' .. #m.message .. ' bytes, not 1..1024:\n' .. dump(ctx):sub(1, 4000))
  local k = ctx.result(3)
  assert(k.code == TOO_LARGE, 'a 10,000-key return answered ' .. tostring(k.code_name) .. ', not TOO_LARGE:\n' .. dump(ctx):sub(1, 4000))
  assert(k.value == S.null, 'a TOO_LARGE result carries a value:\n' .. dump(ctx):sub(1, 4000))
  assert(k.message == 'value over 4096 table entries or 16 KiB of strings',
    'the TOO_LARGE message is ' .. tostring(k.message) .. ':\n' .. dump(ctx):sub(1, 4000))
  for seq, what in pairs({ [4] = 'a 100,000-byte string', [5] = 'a 20 KiB string',
    [6] = 'a list of a 20,000-byte string', [7] = 'a 4,000-element numeric array' }) do
    local r = ctx.result(seq)
    assert(r and r.code == TOO_LARGE, what .. ' answered ' .. tostring(r and r.code_name)
      .. ', not TOO_LARGE:\n' .. dump(ctx):sub(1, 4000))
    assert(r.value == S.null, what .. ' carries a value:\n' .. dump(ctx):sub(1, 4000))
  end
  local results = 0
  for _, e in ipairs(ctx.events()) do
    if e.ev == 'result' then
      results = results + 1
    end
  end
  assert(results == 7, results .. ' RESULTs, not 7:\n' .. dump(ctx):sub(1, 4000))
  S.ok('a 1 MiB error is cut to 1,024 bytes; a 10,000-key return, an over-limit string and a value over the record limit are TOO_LARGE, one RESULT each')
end, '\n[lua]\nraw = true\n')

local BAD_CODE_SHIM = [[
local agent = ...
local M = {}
function M.start()
  return 'bad code start'
end
function M.pump()
  if agent.pump_begin(0, 0, 0) == nil then
    return
  end
  while agent.command() do
    agent.command_done(65536, 'a message the agent drops')
  end
  agent.pump_end()
end
return M
]]
scenario('bad-code', BAD_CODE_SHIM, function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.script('{"commands":[{"op":"MESSAGE"}]}')
  finish(ctx, 'a handler code of 65536')
  local r = ctx.result(1)
  assert(r.code == LUA_ERROR, 'code 65536 answered ' .. tostring(r.code_name) .. ', not LUA_ERROR:\n' .. dump(ctx))
  assert(r.message == 'the handler returned the undefined code 65536',
    'code 65536: message ' .. tostring(r.message) .. ':\n' .. dump(ctx))
  S.ok('a handler code of 65536 is LUA_ERROR naming the code')
end)

for _, profile in ipairs(created) do
  S.remove(profile)
end
assert(ran, 'no scenario named ' .. ONLY)
