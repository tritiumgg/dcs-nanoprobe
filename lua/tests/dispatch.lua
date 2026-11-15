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
local function scenario(name, shim, body, extra_toml, hook_text)
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
  if hook_text then
    S.write(version .. 'lua/hook.lua', hook_text)
  else
    S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  end
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
  ctx.profile = profile
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

-- World commands (S15) through the real agent, the real shim and the strict
-- mock world, with `command-script` as the sidecar. One script run each time
-- the test needs to look at the world between commands; the agent keeps its
-- revisions across the runs.
--
-- * each opcode's APPLIED path and its NOT_BOUND or DCS_REFUSED path;
-- * a spawn answers the group's ID in `object` with a nil value;
-- * a spawn under the name of a group, while paused, then a resume, leaves
--   exactly one group with the new units;
-- * a capture-destroy returns the key table;
-- * a warehouse set reads back, an adjust takes away;
-- * damage sets life; COUNTRIES maps countries to coalitions.
local function json(v)
  if type(v) == 'table' then
    local parts = {}
    if #v > 0 then
      for _, x in ipairs(v) do
        parts[#parts + 1] = json(x)
      end
      return '[' .. table.concat(parts, ',') .. ']'
    end
    local keys = {}
    for k in pairs(v) do
      keys[#keys + 1] = k
    end
    table.sort(keys, function(a, b)
      return tostring(a) < tostring(b)
    end)
    for _, k in ipairs(keys) do
      -- A number key is written as `#<number>`, which command-script reads.
      parts[#parts + 1] = q((type(k) == 'number' and '#' or '') .. tostring(k)) .. ':' .. json(v[k])
    end
    return '{' .. table.concat(parts, ',') .. '}'
  elseif type(v) == 'string' then
    return q(v)
  end
  return tostring(v)
end

scenario('world', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local host = ctx.host
  host:load_mission('first', { paused = true })
  ctx.run(0.5)
  local sim = host.sim
  local server = host.states.server
  local gen = server.__nanoprobe_shim.agent.status().gen
  assert(gen and gen > 0, 'the agent has no mission generation:\n' .. dump(ctx))
  local campaign = 7
  local first = true
  -- Run `commands` as one script; returns the results by seq.
  local function run(commands, what)
    if not first then
      ctx.stop()
    end
    first = false
    for _, c in ipairs(commands) do
      c.gen = c.gen or gen
      c.wait_result = true
    end
    ctx.script(json({ campaign = campaign, commands = commands }))
    finish(ctx, what)
    local results = {}
    for _, e in ipairs(ctx.events()) do
      if e.ev == 'result' then
        results[e.seq] = e
      end
    end
    return results
  end
  local function expect(r, code, what)
    assert(r and r.code == code, what .. ': answered ' .. tostring(r and r.code_name) .. ' '
      .. tostring(r and r.message) .. ', not code ' .. code .. ':\n' .. dump(ctx))
  end
  local NOT_BOUND, DCS_REFUSED, AT_REVISION = 18, 22, 2
  local function group(name, type_, units)
    local list = {}
    for i, u in ipairs(units) do
      list[i] = { name = u, type = type_, x = 1000 + 10 * i, y = 2000, heading = 0 }
    end
    return { name = name, units = list }
  end
  local USA, RUSSIA = 2, 0
  local G, U1, U2, U3 = 'np.g.7.1', 'np.u.7.2', 'np.u.7.3', 'np.u.7.4'

  -- Spawn, order, option; a second order at the same revision is AT_REVISION
  -- until a new spawn clears the order.
  local r = run({
    { op = 'GROUP_SPAWN', entity = 1, rev = 1, country = USA, category = 2, name = G,
      group = group(G, 'M-113', { U1, U2 }) },
    { op = 'GROUP_ORDER', entity = 1, rev = 1, task = { id = 'Hold' } },
    { op = 'GROUP_ORDER', entity = 1, rev = 1, task = { id = 'Hold' } },
    { op = 'GROUP_OPTION', entity = 1, option = 9, value = true },
    { op = 'GROUP_OPTION', entity = 99, option = 9, value = true },
    { op = 'GROUP_ORDER', entity = 99, rev = 1, task = { id = 'Hold' } },
  }, 'spawn, order, option')
  expect(r[1], APPLIED, 'GROUP_SPAWN')
  local g1 = sim.groups[G]
  assert(g1 and #g1.units == 2, 'the group was not spawned')
  assert(r[1].object == g1.id, 'object is ' .. tostring(r[1].object) .. ', not the group ID ' .. g1.id)
  assert(r[1].value == S.null, 'a spawn carries a value:\n' .. dump(ctx))
  expect(r[2], APPLIED, 'GROUP_ORDER')
  local tasks = 0
  for _, order in ipairs(sim.orders) do
    if order.task and order.task.id == 'Hold' and order.group == g1.id then
      tasks = tasks + 1
    end
  end
  assert(tasks == 1, 'the controller got ' .. tasks .. ' tasks, not 1')
  expect(r[3], AT_REVISION, 'GROUP_ORDER again')
  expect(r[4], APPLIED, 'GROUP_OPTION')
  assert(sim.options[g1.id][9] == true, 'the option was not set')
  expect(r[5], NOT_BOUND, 'GROUP_OPTION of no group')
  expect(r[6], NOT_BOUND, 'GROUP_ORDER of no group')

  -- A spawn under the name while paused replaces the group; the order is
  -- sent again after it. A resume leaves exactly the new group.
  local old_ids = {}
  for _, u in ipairs(g1.units) do
    old_ids[#old_ids + 1] = u.id
  end
  r = run({
    { op = 'GROUP_SPAWN', entity = 1, rev = 2, country = USA, category = 2, name = G,
      group = group(G, 'BTR-80', { U1, U3 }) },
    { op = 'GROUP_ORDER', entity = 1, rev = 1, task = { id = 'Hold' } },
  }, 'spawn under the name')
  expect(r[1], APPLIED, 'GROUP_SPAWN under the name')
  expect(r[2], APPLIED, 'GROUP_ORDER after a spawn')
  host:set_pause(false)
  ctx.run(0.5)
  host:set_pause(true)
  local count, last = 0, nil
  for _, group_ in pairs(sim.groups) do
    count = count + 1
    last = group_
  end
  assert(count == 1 and last.name == G and #last.units == 2 and last.units[1].type == 'BTR-80',
    'after a resume the world holds ' .. count .. ' groups, not exactly the new one')
  for _, id in ipairs(old_ids) do
    assert(not sim.units[id], 'an old unit ' .. id .. ' is still in the world')
  end

  -- CAPTURE of the group, then GROUP_CAPTURE_DESTROY: a table for each unit.
  r = run({
    { op = 'CAPTURE', entity = 1 },
    { op = 'GROUP_CAPTURE_DESTROY', entity = 1 },
  }, 'capture')
  expect(r[1], APPLIED, 'CAPTURE')
  expect(r[2], APPLIED, 'GROUP_CAPTURE_DESTROY')
  assert(#r[2].value == 2, 'the capture holds ' .. #r[2].value .. ' units, not 2:\n' .. dump(ctx))
  for _, key in ipairs({ 'name', 'id', 'type', 'x', 'y', 'z', 'heading', 'pitch', 'roll', 'life', 'life0',
    'fuel', 'ammo' }) do
    assert(r[2].value[1][key] ~= nil, 'the captured unit has no ' .. key .. ':\n' .. dump(ctx))
  end
  assert(r[2].value[1].type == 'BTR-80' and r[2].value[1].life0 == 2, 'wrong capture:\n' .. dump(ctx))
  assert(r[1].value[1].name == r[2].value[1].name, 'CAPTURE and the capture-destroy disagree')
  assert(r[2].value[1].ammo[1].type == 'weapons.shells.2A72_30' and r[2].value[1].ammo[1].count == 500
    and r[2].value[1].ammo[1].desc == nil, 'an ammo row is not {type, count}:\n' .. dump(ctx))
  host:set_pause(false)
  ctx.run(0.5)
  host:set_pause(true)
  assert(next(sim.groups) == nil, 'the group is still in the world')
  r = run({
    { op = 'GROUP_CAPTURE_DESTROY', entity = 1 },
    { op = 'GROUP_DESTROY', entity = 1 },
    { op = 'CAPTURE', entity = 1 },
  }, 'capture of nothing')
  expect(r[1], APPLIED, 'GROUP_CAPTURE_DESTROY of nothing')
  assert(r[1].value == S.null, 'a group that is not there has a value')
  expect(r[2], APPLIED, 'GROUP_DESTROY of nothing')
  expect(r[3], NOT_BOUND, 'CAPTURE of nothing')

  -- Statics.
  local S1 = 'np.s.7.5'
  r = run({
    { op = 'STATIC_SPAWN', entity = 5, rev = 1, country = USA, name = S1,
      static = { name = S1, type = 'Barrier', x = 500, y = 600, heading = 0 } },
    { op = 'CAPTURE', entity = 5 },
    { op = 'STATIC_SPAWN', entity = 6, rev = 1, country = USA, name = 'np.s.7.6', static = { type = 'x' } },
    { op = 'STATIC_DESTROY', entity = 5 },
    { op = 'STATIC_DESTROY', entity = 5 },
  }, 'statics')
  expect(r[1], APPLIED, 'STATIC_SPAWN')
  assert(sim.statics[S1] == nil or r[1].object == sim.statics[S1].id, 'the static ID is not in object')
  assert(r[1].object ~= 0 and r[1].value == S.null, 'a static spawn carries no object:\n' .. dump(ctx))
  expect(r[2], APPLIED, 'CAPTURE of a static')
  assert(#r[2].value == 1 and r[2].value[1].name == S1 and r[2].value[1].life == 1
    and r[2].value[1].x == 500 and r[2].value[1].fuel == nil, 'the static capture is wrong:\n' .. dump(ctx))
  expect(r[3], DCS_REFUSED, 'STATIC_SPAWN without a name')
  expect(r[4], APPLIED, 'STATIC_DESTROY')
  expect(r[5], APPLIED, 'STATIC_DESTROY of nothing')

  -- A 30-unit group with a dozen ammo rows each is captured, not TOO_LARGE.
  local BIG, names = 'np.g.7.9', {}
  for i = 1, 30 do
    names[i] = 'np.u.7.big' .. i
  end
  r = run({
    { op = 'GROUP_SPAWN', entity = 9, rev = 1, country = USA, category = 2, name = BIG,
      group = group(BIG, 'T-90', names) },
    { op = 'GROUP_CAPTURE_DESTROY', entity = 9 },
  }, 'a large capture')
  expect(r[1], APPLIED, 'GROUP_SPAWN of 30 units')
  expect(r[2], APPLIED, 'GROUP_CAPTURE_DESTROY of 30 units with 12 ammo rows each')
  assert(#r[2].value == 30 and #r[2].value[1].ammo == 12, 'the large capture is short:\n' .. dump(ctx):sub(1, 3000))
  ctx.run(0.2)
  assert(sim.statics[S1] == nil, 'the static is still in the world')

  -- A group of a type DCS does not know is refused.
  r = run({
    { op = 'GROUP_SPAWN', entity = 8, rev = 1, country = USA, category = 2, name = 'np.g.7.8',
      group = group('np.g.7.8', 'Nonesuch', { 'np.u.7.9' }) },
  }, 'a refused spawn')
  expect(r[1], DCS_REFUSED, 'GROUP_SPAWN of a type DCS lacks')

  -- Airbases: the owner changes with automatic capture off.
  local alpha = sim.airbases[1]
  r = run({
    { op = 'AIRBASE_OWNER', coalition = 2, airbase = 'Alpha' },
    { op = 'AIRBASE_OWNER', coalition = 2, airbase = 'Nowhere' },
    { op = 'AIRBASE_OWNER', coalition = 7, airbase = 'Alpha' },
  }, 'airbase owner')
  expect(r[1], APPLIED, 'AIRBASE_OWNER')
  assert(alpha.coalition == 2 and alpha.auto_capture == false, 'the airbase is not set as the spec says')
  expect(r[2], NOT_BOUND, 'AIRBASE_OWNER of no airbase')
  expect(r[3], DCS_REFUSED, 'AIRBASE_OWNER with a coalition DCS refuses')

  -- Warehouses.
  local gun, jet = 'weapons.missiles.AIM_120C', 0
  r = run({
    { op = 'WAREHOUSE_SET', airbase = 'Alpha',
      stock = { weapon = { [gun] = 8 }, aircraft = { ['F-15C'] = 4 }, liquids = { [jet] = 5000 } } },
    { op = 'WAREHOUSE_ADJUST', airbase = 'Alpha', delta = { weapon = { [gun] = -3 }, liquids = { [jet] = 250 } } },
    { op = 'WAREHOUSE_SET', airbase = 'Nowhere', stock = {} },
    { op = 'WAREHOUSE_ADJUST', airbase = 'Nowhere', delta = {} },
    { op = 'WAREHOUSE_SET', airbase = 'Alpha', stock = { weapon = { [gun] = -1 } } },
  }, 'warehouses')
  expect(r[1], APPLIED, 'WAREHOUSE_SET')
  expect(r[2], APPLIED, 'WAREHOUSE_ADJUST')
  local w = server.Airbase.getByName('Alpha'):getWarehouse()
  local inv = w:getInventory()
  assert(inv.weapon[gun] == 5 and inv.aircraft['F-15C'] == 4 and inv.liquids[0] == 5250
    and w:getItemCount(gun) == 5, 'the inventory does not read back what was set')
  expect(r[3], NOT_BOUND, 'WAREHOUSE_SET of no airbase')
  expect(r[4], NOT_BOUND, 'WAREHOUSE_ADJUST of no airbase')
  expect(r[5], DCS_REFUSED, 'WAREHOUSE_SET of a count DCS refuses')

  -- Damage runs in the hook: the unit's ID from the server state, then the
  -- setter in the mission state (the server state has no `net`).
  local spawn = group('np.g.7.a', 'M-113', { 'np.u.7.b' })
  r = run({
    { op = 'GROUP_SPAWN', entity = 10, rev = 1, country = USA, category = 2, name = 'np.g.7.a', group = spawn },
    { op = 'DAMAGE', entity = 11, life_percent = 50 },
    { op = 'DAMAGE', entity = 77, life_percent = 50 },
    { op = 'DAMAGE', entity = 11, life_percent = 150 },
  }, 'damage')
  expect(r[1], APPLIED, 'GROUP_SPAWN for damage')
  expect(r[2], APPLIED, 'DAMAGE')
  assert(math.abs(sim.by_name['np.u.7.b'].life - 1.5) < 1e-6,
    'the unit has life ' .. sim.by_name['np.u.7.b'].life .. ', not 1.5')
  expect(r[3], NOT_BOUND, 'DAMAGE of no unit')
  expect(r[4], 19, 'DAMAGE with a life over 100')
  assert(sim.life_calls == 1 and sim.dead_checks == 1,
    'the mission state was called ' .. tostring(sim.life_calls) .. ' times, with ' .. tostring(sim.dead_checks)
    .. ' dead checks: a missing unit and a bad life make none')
  sim.dead_ids = { [sim.by_name['np.u.7.b'].mission_id] = true }
  r = run({ { op = 'DAMAGE', entity = 11, life_percent = 20 } }, 'damage of a dead unit')
  expect(r[1], NOT_BOUND, 'DAMAGE of a dead unit')
  assert(sim.life_calls == 1, 'the setter ran on a dead unit')
  sim.dead_ids = nil
  sim.life_raises = true
  r = run({ { op = 'DAMAGE', entity = 11, life_percent = 20 } }, 'damage that raises')
  expect(r[1], DCS_REFUSED, 'DAMAGE that raises')
  sim.life_raises = nil

  -- COUNTRIES: each country's coalition.
  r = run({ { op = 'COUNTRIES' } }, 'countries')
  expect(r[1], APPLIED, 'COUNTRIES')
  local value = r[1].value
  assert(value[tostring(USA)] == 2 and value[tostring(RUSSIA)] == 1, 'the country map is wrong:\n' .. dump(ctx))
  S.ok('world commands: each opcode applies, is NOT_BOUND or DCS_REFUSED as the spec says, and the world holds the result')
end, '\n[lua]\nraw = true\n')

-- A destroy and a replacing spawn write OBJECT_GONE with DESTROYED for each
-- row the agent holds, before the RESULT (spec/commands.md, "Handler
-- arguments").
scenario('world-gone', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local host = ctx.host
  host:load_mission('first', { paused = true })
  local sim = host.sim
  local gen = nil
  local function units(name)
    local ids = {}
    for _, u in ipairs(sim.groups[name].units) do
      ids[#ids + 1] = u.id
    end
    return ids
  end
  local function group(name, units_)
    local list = {}
    for i, u in ipairs(units_) do
      list[i] = { name = u, type = 'M-113', x = 1000 + 10 * i, y = 2000, heading = 0 }
    end
    return { name = name, units = list }
  end
  local campaign, first = 7, true
  local function run(commands, what)
    if not first then
      ctx.stop()
    end
    first = false
    for _, c in ipairs(commands) do
      c.gen, c.wait_result = gen, true
    end
    ctx.script(json({ campaign = campaign, commands = commands }))
    finish(ctx, what)
    return ctx.events()
  end
  ctx.run(0.5)
  gen = host.states.server.__nanoprobe_shim.agent.status().gen
  local G = 'np.g.7.1'
  run({ { op = 'GROUP_SPAWN', entity = 1, rev = 1, country = 2, category = 2, name = G,
    group = group(G, { 'np.u.7.2', 'np.u.7.3' }) } }, 'spawn')
  -- Frames for the births to reach the agent's table.
  ctx.run(1.0)
  local old = units(G)
  local function check(events, ids, result_seq, what)
    local gone, position = {}, {}
    for i, e in ipairs(events) do
      if e.ev == 'gone' then
        gone[e.object] = e.reason
        position[e.object] = i
      end
      if e.ev == 'result' and e.seq == result_seq then
        position.result = i
      end
    end
    for _, id in ipairs(ids) do
      assert(gone[id] == 2, what .. ': object ' .. id .. ' has gone reason ' .. tostring(gone[id])
        .. ', not DESTROYED:\n' .. dump(ctx))
      assert(position[id] < position.result, what .. ': the RESULT came before OBJECT_GONE:\n' .. dump(ctx))
    end
  end
  check(run({ { op = 'GROUP_DESTROY', entity = 1 } }, 'destroy'), old, 1, 'GROUP_DESTROY')
  host:set_pause(false)
  ctx.run(0.5)
  host:set_pause(true)
  assert(sim.groups[G] == nil, 'the group is still in the world')

  -- A spawn under the name of a group the agent holds rows for.
  run({ { op = 'GROUP_SPAWN', entity = 1, rev = 2, country = 2, category = 2, name = G,
    group = group(G, { 'np.u.7.2', 'np.u.7.3' }) } }, 'respawn')
  ctx.run(1.0)
  local before = units(G)
  local events = run({ { op = 'GROUP_SPAWN', entity = 1, rev = 3, country = 2, category = 2, name = G,
    group = group(G, { 'np.u.7.4' }) } }, 'replace')
  check(events, before, 1, 'a replacing GROUP_SPAWN')
  -- The new unit's row is not gone.
  for _, e in ipairs(events) do
    if e.ev == 'gone' then
      for _, u in ipairs(sim.groups[G].units) do
        assert(e.object ~= u.id, 'the new unit ' .. u.id .. ' was written gone')
      end
    end
  end
  -- The destroyed units are pending until DCS removes them, then they go.
  local pending = {}
  for _, id in ipairs(before) do
    pending[id] = true
  end
  host:set_pause(false)
  ctx.run(1.5)
  host:set_pause(true)
  for id in pairs(pending) do
    assert(not sim.units[id], 'the replaced unit ' .. id .. ' is still in the world after a resume')
  end
  local n = 0
  for _ in pairs(sim.groups) do
    n = n + 1
  end
  assert(n == 1 and #sim.groups[G].units == 1, 'a resume leaves ' .. n .. ' groups, not the one new group')

  -- A STATIC_SPAWN under the name of a static the agent holds a row for.
  local S1 = 'np.s.7.9'
  run({ { op = 'STATIC_SPAWN', entity = 9, rev = 1, country = 2, name = S1,
    static = { name = S1, type = 'Barrier', x = 500, y = 600, heading = 0 } } }, 'static spawn')
  ctx.run(1.0)
  local old_static = sim.statics[S1].id
  events = run({ { op = 'STATIC_SPAWN', entity = 9, rev = 2, country = 2, name = S1,
    static = { name = S1, type = 'Barrier', x = 510, y = 600, heading = 0 } } }, 'static replace')
  check(events, { old_static }, 1, 'a replacing STATIC_SPAWN')
  assert(sim.statics[S1].id ~= old_static, 'the replacing static kept the old ID')
  -- A destroyed ID is not found in the shim's table once a new unit reuses its
  -- slot: no existence check reports it alive, so the pending set drains.
  local agent = host.states.server.__nanoprobe_shim.agent
  local alive, real = {}, agent.alive
  agent.alive = function(id, sure)
    if sure then
      alive[id] = true
    end
    return real(id, sure)
  end
  host:set_pause(false)
  ctx.run(1.5)
  host:set_pause(true)
  agent.alive = real
  for _, id in ipairs(before) do
    assert(not alive[id], 'the replaced unit ' .. id .. ' was reported alive after its slot was reused')
  end
  assert(not alive[old_static], 'the replaced static ' .. old_static .. ' was reported alive')
  -- The set has drained: after the last resume and run, nothing is pending
  -- and nothing expired or overflowed.
  local status = agent.status()
  assert(status.pending_destroy == 0 and status.pending_expired == 0 and status.pending_overflow == 0,
    'pending_destroy ' .. tostring(status.pending_destroy) .. ', pending_expired '
    .. tostring(status.pending_expired) .. ', pending_overflow ' .. tostring(status.pending_overflow))
  S.ok('a destroy and a replacing spawn write OBJECT_GONE DESTROYED for the rows held, before the RESULT')
end, '\n[lua]\nraw = true\n')

-- A STATIC_DESTROY handler that returns 4,100 destroyed IDs: the agent takes
-- 4,096, counts 4 as overflow, and the shim forgets only the 4,096 taken.
local MANY = S.read(root .. 'lua/agent/shim.lua'):gsub('  %[22%] = function%(a%)\n', [[
  [22] = function(a)
    local ids = {}
    for i = 1, 4100 do
      local id, slot = 9000000 + i, 1000000 + i
      ids[i] = id
      T.slot_of[id], T.ids[slot] = slot, id
    end
    return APPLIED, nil, nil, ids
  end,
  [2200] = function(a)
]], 1)
scenario('destroyed-overflow', MANY, function(ctx)
  local host = ctx.host
  host:load_mission('first', { paused = true })
  ctx.run(0.5)
  local shim = host.states.server.__nanoprobe_shim
  local agent, T = shim.agent, shim.T
  local gen = agent.status().gen
  local taken, seen, held = nil, nil, nil
  local done, checks = agent.command_done, agent.checks
  agent.command_done = function(...)
    taken = done(...)
    seen = agent.status()
    return taken
  end
  -- The first checks call after the command: the shim has forgotten what it
  -- will, and no existence check has run yet.
  agent.checks = function(...)
    if taken and not held then
      held = 0
      for i = 1, 4100 do
        if T.slot_of[9000000 + i] then
          held = held + 1
        end
      end
    end
    return checks(...)
  end
  local S1 = 'np.s.7.9'
  ctx.script(json({ campaign = 7, commands = {
    { op = 'STATIC_SPAWN', gen = gen, entity = 9, rev = 1, country = 2, name = S1,
      static = { name = S1, type = 'Barrier', x = 500, y = 600, heading = 0 } },
    { op = 'STATIC_DESTROY', gen = gen, entity = 9, wait_result = true },
  } }))
  finish(ctx, 'a destroy of 4,100 IDs')
  assert(taken == 4096, 'command_done took ' .. tostring(taken) .. ' IDs, not 4096')
  assert(seen.pending_overflow == 4 and seen.pending_destroy == 4096,
    'the agent holds ' .. tostring(seen.pending_destroy) .. ' pending and ' .. tostring(seen.pending_overflow)
    .. ' overflow, not 4096 and 4')
  assert(held == 4, 'the shim kept ' .. tostring(held) .. ' of the IDs, not the 4 the agent did not take')
  for i = 4097, 4100 do
    assert(T.slot_of[9000000 + i], 'the shim forgot ID ' .. (9000000 + i) .. ', which the agent did not take')
  end
  S.ok('a destroy of 4,100 IDs: the agent takes 4,096 and counts 4 overflow, and the shim forgets only those taken')
end, '\n[lua]\nraw = true\n')

-- Hook commands (S15, spec/commands.md, "Handlers"): DAMAGE runs in the hook,
-- from the hook's dispatch, in ring order with the shim's commands, under the
-- same budget, and a hook handler that answers nothing gets the "no result"
-- sweep.
local function hook_world(ctx)
  local host = ctx.host
  host:load_mission('first', { paused = true })
  local sim = host.sim
  sim:add_group(2, 2, { name = 'np.g.7.1', units = {
    { name = 'np.u.7.2', type = 'M-113', x = 10, y = 20, heading = 0 } } })
  ctx.run(0.5)
  -- The mission generation: 1 for the first load.
  local gen = 1
  return sim, gen
end

local function run_script(ctx, gen, commands, what)
  for _, c in ipairs(commands) do
    c.gen = gen
  end
  ctx.script(json({ campaign = 7, commands = commands }))
  finish(ctx, what)
end

-- Ring order across a server head, a hook head and a server head.
scenario('hook-order', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, gen = hook_world(ctx)
  run_script(ctx, gen, {
    { op = 'AIRBASE_OWNER', coalition = 2, airbase = 'Alpha' },
    { op = 'DAMAGE', entity = 2, life_percent = 50 },
    { op = 'AIRBASE_OWNER', coalition = 1, airbase = 'Alpha' },
    { op = 'DAMAGE', entity = 2, life_percent = 100 },
  }, 'server, hook, server, hook')
  local order = {}
  for _, e in ipairs(ctx.events()) do
    if e.ev == 'result' then
      assert(e.code == APPLIED, 'seq ' .. e.seq .. ' answered ' .. tostring(e.code_name) .. ' ' .. e.message)
      order[#order + 1] = e.seq
    end
  end
  assert(table.concat(order, ',') == '1,2,3,4', 'the results came in order ' .. table.concat(order, ','))
  assert(sim.airbases[1].coalition == 1 and sim.life_calls == 2, 'the commands did not all run')
  assert(math.abs(sim.by_name['np.u.7.2'].life - 3) < 1e-6, 'the last damage did not set the life')
  S.ok('commands for the shim and the hook run in ring order and each gets one RESULT')
end, '\n[lua]\nraw = true\n')

-- A server handler slower than the budget leaves the hook command for a later
-- frame: they share `commanded_ms`.
local FRAMES_SHIM = [[
local agent = ...
local M = { at = {} }
function M.start()
  return 'frames start'
end
function M.pump()
  if agent.pump_begin(0, 0, 0) == nil then
    return
  end
  while agent.command() do
    local x = 0
    for i = 1, 3000000 do
      x = x + i
    end
    M.at[#M.at + 1] = rawget(_G, '__frame_no')
    agent.command_done(0)
  end
  agent.pump_end()
end
return M
]]
scenario('hook-budget', FRAMES_SHIM, function(ctx)
  local host = ctx.host
  local sim, gen = hook_world(ctx)
  local frame_no, damage_at = 0, nil
  local mission = host.states.mission
  local setter = mission.a_unit_set_life_percentage
  rawset(mission, 'a_unit_set_life_percentage', function(...)
    damage_at = damage_at or frame_no
    return setter(...)
  end)
  ctx.script(json({ campaign = 7, commands = {
    { op = 'MESSAGE', gen = gen },
    { op = 'DAMAGE', entity = 2, life_percent = 50, gen = gen },
  } }))
  assert(ctx.run(30, function()
    return ctx.ended() ~= nil
  end, function()
    frame_no = frame_no + 1
    rawset(host.states.server, '__frame_no', frame_no)
    host:frame(0.01)
  end), 'no end line:\n' .. dump(ctx))
  local at = host.states.server.__nanoprobe_shim.at
  assert(#at == 1 and damage_at, 'the server command or the damage did not run:\n' .. dump(ctx))
  assert(damage_at > at[1], 'the hook command ran in frame ' .. damage_at .. ', the same as the slow server command in '
    .. at[1] .. ': the budget is not shared')
  for seq = 1, 2 do
    assert(ctx.result(seq).code == APPLIED, 'command ' .. seq .. ' was not applied:\n' .. dump(ctx))
  end
  S.ok('a slow server handler leaves the hook command for a later frame: the budget is shared')
end, '\n[budget]\ncommanded_ms = 1.0\n')

-- A hook handler that answers nothing: the agent answers LUA_ERROR "no
-- result", once, and the next command is answered.
local SILENT_HOOK = S.read(root .. 'lua/agent/hook.lua'):gsub('agent%.command_done%(code, message, value%)', 'local _ = code')
scenario('hook-no-result', nil, function(ctx)
  local sim, gen = hook_world(ctx)
  run_script(ctx, gen, {
    { op = 'DAMAGE', entity = 2, life_percent = 50 },
    { op = 'DAMAGE', entity = 2, life_percent = 60 },
  }, 'silent hook handler')
  for seq = 1, 2 do
    local r = ctx.result(seq)
    assert(r and r.code == LUA_ERROR and r.message == 'no result',
      'command ' .. seq .. ' answered ' .. tostring(r and r.code_name) .. ' ' .. tostring(r and r.message))
  end
  local n = 0
  for _, e in ipairs(ctx.events()) do
    if e.ev == 'result' then
      n = n + 1
    end
  end
  assert(n == 2, n .. ' RESULTs, not 2')
  S.ok('a hook handler that answers nothing is answered LUA_ERROR "no result", once each')
end, nil, SILENT_HOOK)

-- DAMAGE needs no shim: the hook reads the server and mission states itself.
scenario('hook-no-shim', nil, function(ctx)
  local sim, gen = hook_world(ctx)
  run_script(ctx, gen, { { op = 'DAMAGE', entity = 2, life_percent = 50 } }, 'DAMAGE with no shim')
  assert(ctx.result(1).code == APPLIED, 'DAMAGE with no shim answered ' .. tostring(ctx.result(1).code_name))
  assert(math.abs(sim.by_name['np.u.7.2'].life - 1.5) < 1e-6, 'the life was not set')
  S.ok('DAMAGE runs in the hook with no shim loaded')
end)

-- SOUND (shim) and PICTURE and PICTURE_STOP (hook), every scope, with the
-- world holding group 1 (`np.g.7.1`) and its unit, entity 2 (`np.u.7.2`).
-- `campaign` 0 is a script that announces no campaign.
local function effect_world(ctx)
  local sim, gen = hook_world(ctx)
  local first = true
  local function run(commands, what, campaign)
    if not first then
      ctx.stop()
    end
    first = false
    for _, c in ipairs(commands) do
      c.gen, c.wait_result = gen, true
    end
    local announced = 7
    if campaign == false then
      announced = nil
    end
    ctx.script(json({ campaign = announced, commands = commands }))
    finish(ctx, what)
    local results = {}
    for _, e in ipairs(ctx.events()) do
      if e.ev == 'result' then
        results[e.seq] = e
      end
    end
    return results
  end
  local function expect(r, code, what)
    assert(r and r.code == code, what .. ': answered ' .. tostring(r and r.code_name) .. ' '
      .. tostring(r and r.message) .. ', not code ' .. code .. ':\n' .. dump(ctx))
  end
  return sim, run, expect
end

scenario('sound', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local NOT_BOUND = 18
  local group_id = sim.groups['np.g.7.1'].id
  local unit_id = sim.by_name['np.u.7.2'].mission_id
  local odd = 'a"b\\c\'d ]] é'
  local r
  -- Campaign 0, before any campaign is announced (the agent keeps it): an
  -- object cannot be named, anything else plays.
  local before = #(sim.sounds or {})
  r = run({
    { op = 'SOUND', scope = 3, target = 1, file = 'group.ogg' },
    { op = 'SOUND', scope = 4, target = 2, file = 'unit.ogg' },
    { op = 'SOUND', scope = 1, file = 'all.ogg' },
  }, 'sound with no campaign', false)
  expect(r[1], NOT_BOUND, 'SOUND to a group with campaign 0')
  expect(r[2], NOT_BOUND, 'SOUND to a unit with campaign 0')
  expect(r[3], APPLIED, 'SOUND to all with campaign 0')
  assert(#sim.sounds == before + 1, 'a sound reached DCS that should not have')
  sim.sounds = {}
  r = run({
    { op = 'SOUND', scope = 1, file = 'all.ogg' },
    { op = 'SOUND', scope = 2, target = 2, file = 'side.ogg' },
    { op = 'SOUND', scope = 5, target = 80, file = 'country.ogg' },
    { op = 'SOUND', scope = 3, target = 1, file = 'group.ogg' },
    { op = 'SOUND', scope = 4, target = 2, file = 'unit.ogg' },
    { op = 'SOUND', scope = 3, target = 99, file = 'nogroup.ogg' },
    { op = 'SOUND', scope = 4, target = 99, file = 'nounit.ogg' },
    { op = 'SOUND', scope = 0, file = 'noscope.ogg' },
    { op = 'SOUND', scope = 6, file = 'noscope.ogg' },
    { op = 'SOUND', scope = 2, target = 4294967296, file = 'big.ogg' },
    { op = 'SOUND', scope = 1, file = odd },
  }, 'sound scopes')
  for seq = 1, 5 do
    expect(r[seq], APPLIED, 'SOUND ' .. seq)
  end
  expect(r[6], NOT_BOUND, 'SOUND to no group')
  expect(r[7], NOT_BOUND, 'SOUND to no unit')
  expect(r[8], BAD_PAYLOAD, 'SOUND with scope 0')
  expect(r[9], BAD_PAYLOAD, 'SOUND with scope 6')
  expect(r[10], BAD_PAYLOAD, 'SOUND to a coalition past 32 bits')
  expect(r[11], APPLIED, 'SOUND with a file name of odd characters')
  local want = {
    { 'outSound', nil, 'all.ogg' },
    { 'outSoundForCoalition', 2, 'side.ogg' },
    { 'outSoundForCountry', 80, 'country.ogg' },
    { 'outSoundForGroup', group_id, 'group.ogg' },
    { 'outSoundForUnit', unit_id, 'unit.ogg' },
    { 'outSound', nil, odd },
  }
  assert(#(sim.sounds or {}) == #want, 'DCS got ' .. #(sim.sounds or {}) .. ' sounds, not ' .. #want)
  for i, w in ipairs(want) do
    local got = sim.sounds[i]
    assert(got.call == w[1] and got.target == w[2] and got.file == w[3],
      'sound ' .. i .. ' is ' .. got.call .. ' ' .. tostring(got.target) .. ' ' .. got.file)
  end

  -- A call DCS refuses is DCS_REFUSED, and a missing file is not an error.
  sim.sound_raises = true
  r = run({ { op = 'SOUND', scope = 1, file = 'x.ogg' } }, 'sound that raises')
  expect(r[1], 22, 'SOUND that raises')
  sim.sound_raises = nil
  r = run({ { op = 'SOUND', scope = 1, file = 'nothing-like-it.ogg' } }, 'sound of a missing file')
  expect(r[1], APPLIED, 'SOUND of a file the mission lacks')

  S.ok('SOUND: each scope calls its DCS form, a missing object or campaign 0 is NOT_BOUND, a bad payload is BAD_PAYLOAD')
end, '\n[lua]\nraw = true\n')

scenario('picture', nil, function(ctx)
  local sim, run, expect = effect_world(ctx)
  local NOT_BOUND = 18
  local group_id = sim.groups['np.g.7.1'].id
  local unit_id = sim.by_name['np.u.7.2'].mission_id
  local function shown(i)
    local p = sim.pictures and sim.pictures[i]
    assert(p, 'no picture call ' .. i)
    return p
  end
  local function full(scope, target, file)
    return { op = 'PICTURE', scope = scope, target = target, file = file, seconds = 12.5, clear_view = 1,
      size = 60, horizontal = 2, vertical = 3, size_units = 4 }
  end
  local r
  -- Campaign 0, before any campaign is announced: no object to name.
  local before = #(sim.pictures or {})
  r = run({ full(3, 1, 'k'), full(4, 2, 'k'), full(1, 0, 'k'), { op = 'PICTURE_STOP' } }, 'picture, no campaign', false)
  expect(r[1], NOT_BOUND, 'PICTURE to a group with campaign 0')
  expect(r[2], NOT_BOUND, 'PICTURE to a unit with campaign 0')
  expect(r[3], APPLIED, 'PICTURE to all with campaign 0')
  expect(r[4], APPLIED, 'PICTURE_STOP with campaign 0')
  assert(#sim.pictures == before + 1 and sim.picture_stops == 1, 'a picture reached DCS that should not have')
  sim.pictures, sim.picture_stops = {}, 0
  r = run({
    full(1, 0, 'Key_1'),
    full(2, 2, 'Key_2'),
    full(5, 80, 'Key_3'),
    full(3, 1, 'Key_4'),
    full(4, 2, 'Key_5.png'),
    full(3, 99, 'Key_6'),
    full(4, 99, 'Key_7'),
    { op = 'PICTURE', scope = 1, file = 'k', seconds = 0, clear_view = 0, size = 1 },
    { op = 'PICTURE_STOP' },
  }, 'picture scopes')
  for _, seq in ipairs({ 1, 2, 3, 4, 5, 8, 9 }) do
    expect(r[seq], APPLIED, 'command ' .. seq)
  end
  expect(r[6], NOT_BOUND, 'PICTURE to no group')
  expect(r[7], NOT_BOUND, 'PICTURE to no unit')
  local want = {
    { 'a_out_picture', nil, 'Key_1' },
    { 'a_out_picture_s', 2, 'Key_2' },
    { 'a_out_picture_c', 80, 'Key_3' },
    { 'a_out_picture_g', group_id, 'Key_4' },
    { 'a_out_picture_u', unit_id, 'Key_5.png' },
  }
  assert(#sim.pictures == #want + 1, 'DCS got ' .. #sim.pictures .. ' pictures, not ' .. (#want + 1))
  for i, w in ipairs(want) do
    local p = shown(i)
    assert(p.call == w[1] and p.target == w[2] and p.file == w[3] and p.seconds == 12.5 and p.clear_view == true
      and p.delay == 0 and p.horizontal == 2 and p.vertical == 3 and p.size == 60 and p.size_units == 4,
      'picture ' .. i .. ' is ' .. p.call .. ' ' .. tostring(p.target) .. ' ' .. p.file .. ' ' .. tostring(p.seconds)
      .. ' ' .. tostring(p.clear_view) .. ' ' .. tostring(p.size))
  end
  local last = shown(6)
  assert(last.seconds == 0 and last.clear_view == false and last.size == 1, 'the last picture differs')
  assert(sim.picture_stops == 1, 'PICTURE_STOP called a_out_picture_stop ' .. tostring(sim.picture_stops) .. ' times')

  -- Files and numbers the agent will not put in source, and a bad scope.
  before = #sim.pictures
  local bad = {}
  for _, file in ipairs({ '', 'a"); x("', "a'b", 'a\\b', 'a\nb', 'é', string.rep('a', 256) }) do
    bad[#bad + 1] = full(1, 0, file)
  end
  bad[#bad + 1] = full(0, 0, 'k')
  bad[#bad + 1] = full(6, 0, 'k')
  bad[#bad + 1] = full(2, 4294967296, 'k')
  local nan = full(1, 0, 'k')
  nan.seconds = -1
  bad[#bad + 1] = nan
  nan = full(1, 0, 'k')
  nan.size = 10000.5
  bad[#bad + 1] = nan
  r = run(bad, 'bad pictures')
  for i = 1, #bad do
    expect(r[i], BAD_PAYLOAD, 'bad PICTURE ' .. i)
  end
  assert(#sim.pictures == before, 'a bad picture reached the mission state')

  -- A mission state with no function, and one that raises.
  sim.picture_raises = true
  r = run({ full(1, 0, 'k'), { op = 'PICTURE_STOP' } }, 'picture that raises')
  expect(r[1], 22, 'PICTURE that raises')
  expect(r[2], APPLIED, 'PICTURE_STOP after a raise')
  sim.picture_raises = nil
  local mission = ctx.host.states.mission
  local keep = mission.a_out_picture_u
  rawset(mission, 'a_out_picture_u', nil)
  r = run({ full(4, 2, 'k') }, 'picture with no function')
  expect(r[1], 22, 'PICTURE with no mission function')
  rawset(mission, 'a_out_picture_u', keep)

  S.ok('PICTURE and PICTURE_STOP: each scope calls its mission function with the Mission Editor order, bad input is BAD_PAYLOAD')
end)

for _, profile in ipairs(created) do
  S.remove(profile)
end
assert(ran, 'no scenario named ' .. ONLY)
