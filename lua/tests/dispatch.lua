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
-- The bytes of a MESSAGE to everyone for 5 s with no text, after its header:
-- the stand-in for a command whose content the test does not read.
local MESSAGE_HEX = '01000000' .. '00000000' .. '0000000000000000' .. '0000000000001440' .. '0000'
local MESSAGE_JSON = '{"op":"MESSAGE","payload_hex":"' .. MESSAGE_HEX .. '"}'

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
  -- The shell opens `out` for the sidecar in the background job, after
  -- os.execute returns. Removing it first keeps a stopped sidecar's lines,
  -- its `end` line among them, from being read as the new one's.
  local function fresh()
    os.remove(out)
  end
  ctx.probe = function()
    fresh()
    assert(os.execute(string.format(
      '%s --map %s --port %d --token-file %s --seconds 60 > %s 2>&1 & echo $! > %s',
      S.quote(PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'), S.quote(out),
      S.quote(pidfile))) == 0, 'could not start link-probe')
  end
  -- `command-script` as the sidecar, for the script `text` (JSON). It stays
  -- attached for `linger_ms` (default 300) after its last RESULT.
  ctx.script = function(text, linger_ms)
    S.write(profile .. 'script.json', text)
    fresh()
    assert(os.execute(string.format(
      '%s --map %s --port %d --token-file %s --script %s --seconds 60 --linger-ms %d > %s 2>&1 & echo $! > %s',
      S.quote(COMMAND_SCRIPT), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'),
      S.quote(profile .. 'script.json'), linger_ms or 300, S.quote(out), S.quote(pidfile))) == 0,
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
  ctx.script('{"campaign":7,"commands":[' .. MESSAGE_JSON .. ']}')
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
  ctx.script('{"commands":[' .. MESSAGE_JSON .. ',' .. MESSAGE_JSON .. ']}')
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
    -- A sampling pump that plans nothing and spends past the slice: its time
    -- is the row tiers' debt, so the gate closes and later frames are forced
    -- for the waiting command. A pump that plans the enumeration and reads
    -- none of it never ends it, and the births' and the enumeration's
    -- balance pays such a pump whole (spec/sampling.md, "Tiers"): the debt
    -- stays 0, so the gate does not close for it and no frame is forced.
    local start = os.clock()
    repeat
    until os.clock() - start >= 0.002
  end
  agent.pump_end()
end
return M
]]
local function never_reached(shim)
  return function(ctx)
    ctx.host:load_mission('first', { paused = true })
    ctx.script('{"commands":[' .. MESSAGE_JSON .. ']}')
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
  -- With no row owed, births and the enumeration take a batch only on a
  -- balance that pays it (spec/sampling.md, "Tiers"): the 2 ms pumps keep the
  -- share and nothing is forgiven. 11 s more gives one 10 s `sampling` window.
  -- The 2 % on the share is the carry across a window's edges: the 250 µs
  -- balance and the 150 µs credit, 400 µs in 10 s, 1.3 % of 0.3 %.
  ctx.run(11)
  local windows = 0
  for _, line in ipairs(S.lines(ctx.profile .. 'nanoprobe/logs/agent.log')) do
    local ok, e = pcall(S.json, line)
    local d = ok and type(e) == 'table' and e.msg == 'sampling' and e.data
    if d then
      windows = windows + 1
      assert(d.maintenance_forgiven_us == 0,
        'the balance forgave ' .. tostring(d.maintenance_forgiven_us) .. ' µs with no stall')
      assert(d.share <= 0.003 * 1.02, 'share ' .. tostring(d.share) .. ' against 0.003')
    end
  end
  assert(windows >= 1, 'no sampling report in the agent log')
  S.ok('a forced pump that calls pump_begin and no command() leaves the head command NOT_READY, once, '
    .. 'within the share and with nothing forgiven')
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
  ctx.script('{"commands":[' .. MESSAGE_JSON .. ']}')
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
  local G, U1, U2, U3 = 'np!hGx1', 'np.hGx2', 'np.hGx3', 'np.hGx4'

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
    'fuel' }) do
    assert(r[2].value[1][key] ~= nil, 'the captured unit has no ' .. key .. ':\n' .. dump(ctx))
  end
  assert(r[2].value[1].type == 'BTR-80' and r[2].value[1].life0 == 2, 'wrong capture:\n' .. dump(ctx))
  assert(r[1].value[1].name == r[2].value[1].name, 'CAPTURE and the capture-destroy disagree')
  assert(r[2].value[1].ammo == nil, 'the capture holds an ammo key:\n' .. dump(ctx))
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
  local S1 = 'np#hGx5'
  r = run({
    { op = 'STATIC_SPAWN', entity = 5, rev = 1, country = USA, name = S1,
      static = { name = S1, type = 'Barrier', x = 500, y = 600, heading = 0 } },
    { op = 'CAPTURE', entity = 5 },
    { op = 'STATIC_SPAWN', entity = 6, rev = 1, country = USA, name = 'np#hGx6', static = { type = 'x' } },
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

  -- A 30-unit group is captured, not TOO_LARGE.
  local BIG, names = 'np!hGx9', {}
  for i = 1, 30 do
    names[i] = 'np.hGxbig' .. i
  end
  r = run({
    { op = 'GROUP_SPAWN', entity = 9, rev = 1, country = USA, category = 2, name = BIG,
      group = group(BIG, 'T-90', names) },
    { op = 'GROUP_CAPTURE_DESTROY', entity = 9 },
  }, 'a large capture')
  expect(r[1], APPLIED, 'GROUP_SPAWN of 30 units')
  expect(r[2], APPLIED, 'GROUP_CAPTURE_DESTROY of 30 units')
  assert(#r[2].value == 30, 'the large capture is short:\n' .. dump(ctx):sub(1, 3000))
  ctx.run(0.2)
  assert(sim.statics[S1] == nil, 'the static is still in the world')

  -- A group of a type DCS does not know is refused.
  r = run({
    { op = 'GROUP_SPAWN', entity = 8, rev = 1, country = USA, category = 2, name = 'np!hGx8',
      group = group('np!hGx8', 'Nonesuch', { 'np.hGx9' }) },
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
  local spawn = group('np!hGxA', 'M-113', { 'np.hGxB' })
  r = run({
    { op = 'GROUP_SPAWN', entity = 10, rev = 1, country = USA, category = 2, name = 'np!hGxA', group = spawn },
    { op = 'DAMAGE', entity = 11, life_percent = 50 },
    { op = 'DAMAGE', entity = 77, life_percent = 50 },
    { op = 'DAMAGE', entity = 11, life_percent = 150 },
  }, 'damage')
  expect(r[1], APPLIED, 'GROUP_SPAWN for damage')
  expect(r[2], APPLIED, 'DAMAGE')
  assert(math.abs(sim.by_name['np.hGxB'].life - 1.5) < 1e-6,
    'the unit has life ' .. sim.by_name['np.hGxB'].life .. ', not 1.5')
  expect(r[3], NOT_BOUND, 'DAMAGE of no unit')
  expect(r[4], 19, 'DAMAGE with a life over 100')
  assert(sim.life_calls == 1 and sim.dead_checks == 1,
    'the mission state was called ' .. tostring(sim.life_calls) .. ' times, with ' .. tostring(sim.dead_checks)
    .. ' dead checks: a missing unit and a bad life make none')
  sim.dead_ids = { [sim.by_name['np.hGxB'].mission_id] = true }
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
  local campaign = 7
  local function run(commands, what)
    -- The last script, or the sidecar `wait` left attached.
    ctx.stop()
    for _, c in ipairs(commands) do
      c.gen, c.wait_result = gen, true
    end
    ctx.script(json({ campaign = campaign, commands = commands }))
    finish(ctx, what)
    return ctx.events()
  end
  local shim = host.states.server.__nanoprobe_shim
  local agent = shim.agent
  -- Frames until `done` holds. A frame costs more CPU under load, so a wait
  -- is for a state, never for a fixed time. A detached agent samples
  -- nothing (spec/sampling.md), and a script's sidecar leaves 300 ms after
  -- its last RESULT, so a wait on the sampler (`attached`) first starts a
  -- sidecar that stays until the next `run`.
  local function wait(what, done, attached)
    if attached then
      ctx.stop()
      ctx.script('{"commands":[]}', 60000)
    end
    assert(ctx.run(30, function()
      return (not attached or agent.status().attached) and done()
    end), what .. ':\n' .. dump(ctx))
  end
  -- Whether the agent holds a row for each of `ids`: the shim keeps a slot
  -- only for an object the agent took (lua/agent/shim.lua, `read`). The
  -- agent writes OBJECT_GONE only for the rows it holds (spec/commands.md,
  -- "Handler arguments"), so each destroy waits for its rows first.
  local function held(ids)
    for _, id in ipairs(ids) do
      if not shim.T.slot_of[id] then
        return false
      end
    end
    return true
  end
  wait('the agent has no mission generation', function()
    gen = agent.status().gen
    return gen ~= nil and gen > 0
  end)
  local G = 'np!hGx1'
  run({ { op = 'GROUP_SPAWN', entity = 1, rev = 1, country = 2, category = 2, name = G,
    group = group(G, { 'np.hGx2', 'np.hGx3' }) } }, 'spawn')
  wait('the spawned units have no rows', function()
    return held(units(G))
  end, true)
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
  wait('the group is still in the world after a resume', function()
    return sim.groups[G] == nil
  end)
  host:set_pause(true)

  -- A spawn under the name of a group the agent holds rows for.
  run({ { op = 'GROUP_SPAWN', entity = 1, rev = 2, country = 2, category = 2, name = G,
    group = group(G, { 'np.hGx2', 'np.hGx3' }) } }, 'respawn')
  wait('the respawned units have no rows', function()
    return held(units(G))
  end, true)
  local before = units(G)
  local events = run({ { op = 'GROUP_SPAWN', entity = 1, rev = 3, country = 2, category = 2, name = G,
    group = group(G, { 'np.hGx4' }) } }, 'replace')
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
  wait('a replaced unit is still in the world after a resume', function()
    for id in pairs(pending) do
      if sim.units[id] then
        return false
      end
    end
    return true
  end)
  host:set_pause(true)
  local n = 0
  for _ in pairs(sim.groups) do
    n = n + 1
  end
  assert(n == 1 and #sim.groups[G].units == 1, 'a resume leaves ' .. n .. ' groups, not the one new group')

  -- A STATIC_SPAWN under the name of a static the agent holds a row for.
  local S1 = 'np#hGx9'
  run({ { op = 'STATIC_SPAWN', entity = 9, rev = 1, country = 2, name = S1,
    static = { name = S1, type = 'Barrier', x = 500, y = 600, heading = 0 } } }, 'static spawn')
  wait('the static has no row', function()
    return held({ sim.statics[S1].id })
  end, true)
  local old_static = sim.statics[S1].id
  events = run({ { op = 'STATIC_SPAWN', entity = 9, rev = 2, country = 2, name = S1,
    static = { name = S1, type = 'Barrier', x = 510, y = 600, heading = 0 } } }, 'static replace')
  check(events, { old_static }, 1, 'a replacing STATIC_SPAWN')
  assert(sim.statics[S1].id ~= old_static, 'the replacing static kept the old ID')
  -- A destroyed ID is not found in the shim's table once a new unit reuses its
  -- slot: no existence check reports it alive, so the pending set drains.
  local alive, real = {}, agent.alive
  agent.alive = function(id, sure)
    if sure then
      alive[id] = true
    end
    return real(id, sure)
  end
  host:set_pause(false)
  wait('the pending-destroy set did not drain', function()
    return agent.status().pending_destroy == 0
  end, true)
  host:set_pause(true)
  agent.alive = real
  for _, id in ipairs(before) do
    assert(not alive[id], 'the replaced unit ' .. id .. ' was reported alive after its slot was reused')
  end
  assert(not alive[old_static], 'the replaced static ' .. old_static .. ' was reported alive')
  -- The set has drained, and nothing expired or overflowed.
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
  local S1 = 'np#hGx9'
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
  sim:add_group(2, 2, { name = 'np!hGx1', units = {
    { name = 'np.hGx2', type = 'M-113', x = 10, y = 20, heading = 0 } } })
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
  assert(math.abs(sim.by_name['np.hGx2'].life - 3) < 1e-6, 'the last damage did not set the life')
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
    { op = 'MESSAGE', gen = gen, payload_hex = MESSAGE_HEX },
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
  assert(math.abs(sim.by_name['np.hGx2'].life - 1.5) < 1e-6, 'the life was not set')
  S.ok('DAMAGE runs in the hook with no shim loaded')
end)

-- SOUND (shim) and PICTURE and PICTURE_STOP (hook), every scope, with the
-- world holding group 1 (`np!hGx1`) and its unit, entity 2 (`np.hGx2`).
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
  local group_id = sim.groups['np!hGx1'].id
  local unit_id = sim.by_name['np.hGx2'].mission_id
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
  local group_id = sim.groups['np!hGx1'].id
  local unit_id = sim.by_name['np.hGx2'].mission_id
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

-- S16 commands: marks, drawings, menu items, MESSAGE, EXTENSION_REMOVE and EVAL
-- (shim), CHAT and MISSION_LOAD (hook). The layouts have no builder in
-- `command-script`, so a test writes the bytes after the header itself and
-- passes them as `payload_hex`.
local PACK = {}

local function le(v, n)
  local out = {}
  for i = 1, n do
    out[i] = string.char(v % 256)
    v = math.floor(v / 256)
  end
  return table.concat(out)
end

function PACK.u32(v)
  return le(v, 4)
end

function PACK.u64(v)
  return le(v, 8)
end

-- An IEEE 754 double, little-endian. NaN and the infinities are written by
-- their bytes; the rest by their exponent and mantissa.
function PACK.f64(x)
  if x ~= x then
    return '\0\0\0\0\0\0\248\127'
  elseif x == math.huge then
    return '\0\0\0\0\0\0\240\127'
  elseif x == -math.huge then
    return '\0\0\0\0\0\0\240\255'
  elseif x == 0 then
    return string.rep('\0', 8)
  end
  local sign = 0
  if x < 0 then
    sign, x = 1, -x
  end
  local m, e = math.frexp(x)
  local biased = e + 1022
  local mant = math.floor((m * 2 - 1) * 2 ^ 52)
  local out = {}
  for i = 1, 6 do
    out[i] = string.char(mant % 256)
    mant = math.floor(mant / 256)
  end
  out[7] = string.char(mant % 16 + (biased % 16) * 16)
  out[8] = string.char(sign * 128 + math.floor(biased / 16))
  return table.concat(out)
end

function PACK.str(text)
  return le(#text, 2) .. text
end

function PACK.lv(v)
  local t = type(v)
  if v == nil then
    return '\0'
  elseif t == 'boolean' then
    return v and '\2' or '\1'
  elseif t == 'number' then
    return '\3' .. PACK.f64(v)
  elseif t == 'string' then
    return '\4' .. le(#v, 4) .. v
  end
  local parts, n = {}, 0
  for i, item in ipairs(v) do
    parts[#parts + 1] = PACK.lv(i) .. PACK.lv(item)
    n = n + 1
  end
  local keys = {}
  for k in pairs(v) do
    if type(k) == 'string' then
      keys[#keys + 1] = k
    end
  end
  table.sort(keys)
  for _, k in ipairs(keys) do
    parts[#parts + 1] = PACK.lv(k) .. PACK.lv(v[k])
    n = n + 1
  end
  return '\5' .. le(n, 4) .. table.concat(parts)
end

local function hexof(bytes)
  return (bytes:gsub('.', function(c)
    return string.format('%02x', c:byte())
  end))
end

-- A command of the script, `op` with the bytes after its header.
local function with(op, header, bytes)
  header.op = op
  header.payload_hex = hexof(bytes)
  return header
end

local function mark(entity, rev, scope, target, text, read_only)
  return with('MARK_SET', { entity = entity, rev = rev }, PACK.u32(scope) .. PACK.u32(read_only == false and 0 or 1)
    .. PACK.u64(target) .. PACK.f64(10.5) .. PACK.f64(20) .. PACK.f64(-30.25) .. PACK.str(text))
end

local function drawing(entity, rev, scope, target, args)
  return with('DRAWING_SET', { entity = entity, rev = rev }, PACK.u32(scope) .. PACK.u32(7)
    .. PACK.u64(target) .. PACK.lv(args))
end

local function menu(entity, rev, parent, scope, target, submenu, text)
  return with('MENU_SET', { entity = entity, rev = rev }, PACK.u64(parent) .. PACK.u64(target)
    .. PACK.u32(scope) .. PACK.u32(submenu and 1 or 0) .. PACK.str(text))
end

local function message(scope, target, seconds, text, clear)
  return with('MESSAGE', {}, PACK.u32(scope) .. PACK.u32(clear and 1 or 0) .. PACK.u64(target)
    .. PACK.f64(seconds) .. PACK.str(text))
end

local function chat(player, text)
  return with('CHAT', {}, PACK.u32(player) .. PACK.u32(0) .. PACK.str(text))
end

local function count(t)
  local n = 0
  for _ in pairs(t) do
    n = n + 1
  end
  return n
end

local function only(t)
  local id, value
  for k, v in pairs(t) do
    assert(id == nil, 'more than one entry')
    id, value = k, v
  end
  return id, value
end

local function joined(list)
  return table.concat(list, ',')
end

scenario('marks', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local NOT_BOUND = 18
  local group_id = sim.groups['np!hGx1'].id
  local r

  -- A mark to everyone, then the same entity again: the old DCS mark goes
  -- and the new one has a new ID, from the shim's own range.
  r = run({ mark(10, 1, 1, 0, 'alpha') }, 'a mark to all')
  expect(r[1], APPLIED, 'MARK_SET to all')
  local first, m = only(sim.marks)
  assert(first >= 2 ^ 30, 'the mark ID ' .. first .. ' is below 2^30')
  assert(m.text == 'alpha' and m.read_only == true and m.coalition == 255 and m.group == nil, 'the mark differs')
  assert(m.point.x == 10.5 and m.point.y == 20 and m.point.z == -30.25, 'the mark is at ' .. m.point.x)
  r = run({ mark(10, 2, 1, 0, 'beta', false) }, 'a replaced mark')
  expect(r[1], APPLIED, 'MARK_SET replacing')
  local second
  second, m = only(sim.marks)
  assert(second ~= first and second >= 2 ^ 30, 'the replacement kept the ID ' .. first)
  assert(m.text == 'beta' and m.read_only == false, 'the replacement differs')

  -- A coalition and a group.
  r = run({
    mark(11, 1, 2, 2, 'blue'),
    mark(12, 1, 3, 1, 'group'),
    mark(13, 1, 3, 99, 'nogroup'),
    mark(14, 1, 4, 2, 'unit scope'),
    mark(15, 1, 5, 2, 'country scope'),
    mark(16, 1, 0, 0, 'no scope'),
    mark(17, 1, 2, 4294967296, 'big coalition'),
  }, 'mark scopes')
  expect(r[1], APPLIED, 'MARK_SET to a coalition')
  expect(r[2], APPLIED, 'MARK_SET to a group')
  expect(r[3], NOT_BOUND, 'MARK_SET to no group')
  for seq = 4, 7 do
    expect(r[seq], BAD_PAYLOAD, 'MARK_SET with a scope or target the opcode does not take ' .. seq)
  end
  assert(count(sim.marks) == 3, 'DCS holds ' .. count(sim.marks) .. ' marks, not 3')
  local by_text = {}
  for _, mark_ in pairs(sim.marks) do
    by_text[mark_.text] = mark_
  end
  assert(by_text.blue.coalition == 2, 'the coalition mark is for ' .. tostring(by_text.blue.coalition))
  assert(by_text.group.group == group_id, 'the group mark is for ' .. tostring(by_text.group.group))

  -- A mark DCS refuses.
  sim.mark_raises = true
  r = run({ mark(18, 1, 1, 0, 'refused') }, 'a refused mark')
  expect(r[1], 22, 'MARK_SET that raises')
  sim.mark_raises = nil
  assert(count(sim.marks) == 3, 'a refused mark is in DCS')

  -- Removal: the held mark goes, one nobody holds is APPLIED.
  r = run({
    { op = 'MARK_REMOVE', entity = 10 },
    { op = 'MARK_REMOVE', entity = 10 },
    { op = 'MARK_REMOVE', entity = 999 },
  }, 'mark removal')
  for seq = 1, 3 do
    expect(r[seq], APPLIED, 'MARK_REMOVE ' .. seq)
  end
  assert(count(sim.marks) == 2 and sim.marks[second] == nil, 'the removed mark is in DCS')

  -- Drawings: coalition -1 for all, the arguments unpacked after the ID.
  local points = { { x = 1, y = 2, z = 3 }, { x = 4, y = 5, z = 6 }, 'extra' }
  r = run({
    drawing(20, 1, 1, 0, points),
    drawing(21, 1, 2, 1, points),
    drawing(22, 1, 3, 1, points),
    drawing(23, 1, 4, 1, points),
    drawing(24, 1, 1, 0, {}),
  }, 'drawings')
  expect(r[1], APPLIED, 'DRAWING_SET to all')
  expect(r[2], APPLIED, 'DRAWING_SET to a coalition')
  expect(r[3], BAD_PAYLOAD, 'DRAWING_SET to a group')
  expect(r[4], BAD_PAYLOAD, 'DRAWING_SET to a unit')
  expect(r[5], APPLIED, 'DRAWING_SET with no arguments')
  local drawn = {}
  for id, d in pairs(sim.marks) do
    if d.drawing then
      drawn[#drawn + 1] = { id = id, d = d }
    end
  end
  assert(#drawn == 3, 'DCS holds ' .. #drawn .. ' drawings, not 3')
  table.sort(drawn, function(x, y)
    return x.id < y.id
  end)
  assert(drawn[1].d.shape == 7 and drawn[1].d.coalition == 255 and #drawn[1].d.args == 3
    and drawn[1].d.args[2].z == 6 and drawn[1].d.args[3] == 'extra', 'the drawing to all differs')
  assert(drawn[2].d.coalition == 1, 'the drawing for a coalition is for ' .. tostring(drawn[2].d.coalition))
  local before = drawn[1].id
  r = run({ drawing(20, 2, 1, 0, points) }, 'a replaced drawing')
  expect(r[1], APPLIED, 'DRAWING_SET replacing')
  assert(sim.marks[before] == nil, 'the replaced drawing is still in DCS')
  assert(count(sim.marks) == 5, 'DCS holds ' .. count(sim.marks) .. ' marks and drawings, not 5')
  r = run({ { op = 'DRAWING_REMOVE', entity = 20 }, { op = 'DRAWING_REMOVE', entity = 20 } }, 'drawing removal')
  expect(r[1], APPLIED, 'DRAWING_REMOVE')
  expect(r[2], APPLIED, 'DRAWING_REMOVE of nothing')
  assert(count(sim.marks) == 4, 'DCS holds ' .. count(sim.marks) .. ' marks and drawings, not 4')

  -- A new mission generation: the shim forgets what it held.
  local shim = ctx.host.states.server.__nanoprobe_shim
  assert(count(shim.marks) > 0, 'the shim holds no marks')
  ctx.stop()
  ctx.host:load_mission('second', { paused = true })
  ctx.run(0.5, function()
    return count(shim.marks) == 0
  end)
  assert(count(shim.marks) == 0 and count(shim.menus) == 0, 'the shim kept marks across a mission load')
  S.ok('MARK_SET, DRAWING_SET and their removals: scopes, replacement under a new ID, NOT_BOUND, BAD_PAYLOAD, DCS_REFUSED, a load forgets them')
end, '\n[lua]\nraw = true\n')

scenario('menus', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local NOT_BOUND = 18
  local group_id = sim.groups['np!hGx1'].id
  local shim = ctx.host.states.server.__nanoprobe_shim
  local r
  local function list(key)
    return joined(sim:menu_list(key or 'all'))
  end

  -- Top (30) holds A (31, a command), Sub (32) with B (33), and C (34).
  r = run({
    menu(30, 1, 0, 1, 0, true, 'Top'),
    menu(31, 1, 30, 1, 0, false, 'A'),
    menu(32, 1, 30, 1, 0, true, 'Sub'),
    menu(33, 1, 32, 1, 0, false, 'B'),
    menu(34, 1, 30, 1, 0, false, 'C'),
  }, 'a menu tree')
  for seq = 1, 5 do
    expect(r[seq], APPLIED, 'MENU_SET ' .. seq)
  end
  assert(list() == 'Top,Top/A,Top/Sub,Top/Sub/B,Top/C', 'the menu is ' .. list())

  -- A replaced submenu is added again with every item the shim holds below
  -- it, in the order they were first set, at every depth.
  r = run({ menu(30, 2, 0, 1, 0, true, 'Top2') }, 'a replaced root submenu')
  expect(r[1], APPLIED, 'MENU_SET replacing Top')
  assert(list() == 'Top2,Top2/A,Top2/Sub,Top2/Sub/B,Top2/C', 'the menu is ' .. list())
  r = run({ menu(32, 2, 30, 1, 0, true, 'Sub2') }, 'a replaced inner submenu')
  expect(r[1], APPLIED, 'MENU_SET replacing Sub')
  assert(list() == 'Top2,Top2/A,Top2/C,Top2/Sub2,Top2/Sub2/B', 'the menu is ' .. list())
  r = run({ menu(31, 2, 30, 1, 0, false, 'A2') }, 'a replaced command')
  expect(r[1], APPLIED, 'MENU_SET replacing A')
  assert(list() == 'Top2,Top2/C,Top2/Sub2,Top2/Sub2/B,Top2/A2', 'the menu is ' .. list())

  -- A command calls the agent's `menu` with its entity, when the agent has
  -- one, and never raises into DCS.
  local chosen = {}
  sim:menu_select('all', { 'Top2', 'A2' })
  shim.agent.menu = function(entity)
    chosen[#chosen + 1] = entity
  end
  sim:menu_select('all', { 'Top2', 'A2' })
  sim:menu_select('all', { 'Top2', 'Sub2', 'B' })
  assert(joined(chosen) == '31,33', 'the agent heard ' .. joined(chosen))
  shim.agent.menu = function()
    error('the agent raised')
  end
  sim:menu_select('all', { 'Top2', 'C' })
  shim.agent.menu = nil

  -- A parent that is missing, a command, or in another scope; a group that is
  -- not there; scopes the opcode does not take.
  r = run({
    menu(40, 1, 999, 1, 0, false, 'orphan'),
    menu(41, 1, 31, 1, 0, false, 'under a command'),
    menu(42, 1, 30, 2, 2, false, 'other scope'),
    menu(43, 1, 0, 3, 99, true, 'no group'),
    menu(44, 1, 0, 4, 2, false, 'unit scope'),
    menu(45, 1, 0, 5, 2, false, 'country scope'),
    menu(46, 1, 30, 1, 0, true, 'Inner'),
    menu(30, 3, 46, 1, 0, true, 'under its own child'),
  }, 'bad menu items')
  for _, seq in ipairs({ 1, 2, 3, 4, 8 }) do
    expect(r[seq], NOT_BOUND, 'MENU_SET ' .. seq)
  end
  expect(r[5], BAD_PAYLOAD, 'MENU_SET to a unit')
  expect(r[6], BAD_PAYLOAD, 'MENU_SET to a country')
  expect(r[7], APPLIED, 'MENU_SET of a submenu under Top')
  assert(list() == 'Top2,Top2/C,Top2/Sub2,Top2/Sub2/B,Top2/A2,Top2/Inner', 'the menu is ' .. list())

  -- A coalition and a group scope each have their own tree.
  r = run({
    menu(50, 1, 0, 2, 2, true, 'Blue'),
    menu(51, 1, 50, 2, 2, false, 'Blue command'),
    menu(52, 1, 0, 3, 1, true, 'Group'),
    menu(53, 1, 52, 3, 1, false, 'Group command'),
    menu(54, 1, 52, 3, 2, false, 'a parent for another coalition or group'),
  }, 'scoped menus')
  for seq = 1, 4 do
    expect(r[seq], APPLIED, 'scoped MENU_SET ' .. seq)
  end
  expect(r[5], NOT_BOUND, 'MENU_SET under a parent of another target')
  assert(list('c2') == 'Blue,Blue/Blue command', 'the blue menu is ' .. list('c2'))
  assert(list('g' .. group_id) == 'Group,Group/Group command', 'the group menu is ' .. list('g' .. group_id))
  assert(list() == 'Top2,Top2/C,Top2/Sub2,Top2/Sub2/B,Top2/A2,Top2/Inner', 'the all menu changed')

  -- A group submenu set while the agent held no row for the group carries
  -- the platform name; a later set carries the group ID and an empty name.
  -- The shim hands the commands to itself through the agent's ring, so the
  -- test chooses the arguments.
  do
    local agent = shim.agent
    local real_command, real_done = agent.command, agent.command_done
    local queue, done = {}, {}
    agent.command = function()
      local c = table.remove(queue, 1)
      if c then
        return 29, c
      end
    end
    agent.command_done = function(code, message, value)
      done[#done + 1] = { code = code, message = message, value = value }
      return 0
    end
    local function send(c)
      c.scope, c.target = 3, 1
      queue[1] = c
      done = {}
      ctx.stop()
      shim.pump()
      return done[1]
    end
    local by_name = { name = 'np!hGx1' }
    local by_id = { name = '', group_id = group_id }
    local function set(entity, parent, extra, text, submenu)
      local c = { entity = entity, parent = parent, submenu = submenu, text = text }
      for k, v in pairs(extra) do
        c[k] = v
      end
      return send(c)
    end
    expect(set('80', '0', by_name, 'GS', true), APPLIED, 'a group submenu by name')
    expect(set('81', '80', by_name, 'GS child', false), APPLIED, 'a group child by name')
    expect(set('83', '80', by_id, 'GS child by ID', false), APPLIED, 'a child by group ID under a submenu set by name')
    local again = set('80', '0', by_id, 'GS2', true)
    expect(again, APPLIED, 're-setting the submenu by group ID')
    assert(again.value == nil, 'the re-set dropped children')
    expect(set('82', '80', by_id, 'GS child 2', false), APPLIED, 'a child by group ID')
    assert(list('g' .. group_id) == 'Group,Group/Group command,GS2,GS2/GS child,GS2/GS child by ID,GS2/GS child 2',
      'the group menu is ' .. list('g' .. group_id))
    expect(send({ entity = '80', parent = '0', submenu = true, text = 'x', name = '', group_id = group_id }),
      APPLIED, 'a plain re-set')
    agent.command, agent.command_done = real_command, real_done
    r = run({ { op = 'MENU_REMOVE', entity = 80 } }, 'removal of the group submenu')
    expect(r[1], APPLIED, 'MENU_REMOVE of the group submenu')
  end

  -- A refused add leaves nothing held.
  sim.menu_raises = true
  r = run({ menu(60, 1, 0, 1, 0, false, 'refused') }, 'a refused menu item')
  expect(r[1], 22, 'MENU_SET that raises')
  sim.menu_raises = nil
  assert(shim.menus['60'] == nil, 'the shim holds a refused item')

  -- Removal: a submenu takes the items below it out of the shim's table.
  assert(shim.menus['33'] ~= nil, 'the shim does not hold B')
  r = run({
    { op = 'MENU_REMOVE', entity = 32 },
    { op = 'MENU_REMOVE', entity = 33 },
    { op = 'MENU_REMOVE', entity = 999 },
  }, 'menu removal')
  for seq = 1, 3 do
    expect(r[seq], APPLIED, 'MENU_REMOVE ' .. seq)
  end
  assert(list() == 'Top2,Top2/C,Top2/A2,Top2/Inner', 'the menu is ' .. list())
  assert(shim.menus['32'] == nil and shim.menus['33'] == nil, 'the shim kept the removed items')
  r = run({ menu(30, 3, 0, 1, 0, true, 'Top3') }, 'a replaced root submenu after a removal')
  expect(r[1], APPLIED, 'MENU_SET replacing Top again')
  assert(list() == 'Top3,Top3/A2,Top3/C,Top3/Inner', 'the menu is ' .. list())

  -- A parent removed with MENU_REMOVE takes a held child with it.
  r = run({ { op = 'MENU_REMOVE', entity = 30 }, { op = 'MENU_REMOVE', entity = 52 } }, 'menu root removal')
  expect(r[1], APPLIED, 'MENU_REMOVE of the root')
  expect(r[2], APPLIED, 'MENU_REMOVE of the group root')
  assert(list() == '' and list('g' .. group_id) == '', 'the menus are ' .. list() .. ' and ' .. list('g' .. group_id))
  assert(shim.menus['31'] == nil and shim.menus['34'] == nil and shim.menus['53'] == nil,
    'the shim kept items below a removed submenu')
  -- A replacement whose held child DCS will not add again forgets the child
  -- and names it in the value, so the sidecar sends it again.
  run({
    menu(70, 1, 0, 1, 0, true, 'R'),
    menu(71, 1, 70, 1, 0, true, 'S'),
    menu(72, 1, 71, 1, 0, false, 'T'),
    menu(73, 1, 70, 1, 0, false, 'U'),
  }, 'a tree whose child will be refused')
  local commands = ctx.host.states.server.missionCommands
  local add = commands.addCommand
  commands.addCommand = function(text, ...)
    if text == 'T' then
      error('mock: refused')
    end
    return add(text, ...)
  end
  r = run({ menu(70, 2, 0, 1, 0, true, 'R2') }, 'a replacement that loses a child')
  commands.addCommand = add
  expect(r[1], APPLIED, 'MENU_SET replacing R')
  assert(joined(r[1].value or {}) == '72', 'the dropped items are ' .. joined(r[1].value or {}) .. ':\n' .. dump(ctx))
  assert(list() == 'R2,R2/S,R2/U', 'the menu is ' .. list())
  assert(shim.menus['72'] == nil and shim.menus['71'] ~= nil, 'the shim holds the dropped item')
  S.ok('MENU_SET and MENU_REMOVE: replacement re-adds held children in order, NOT_BOUND parents, scopes, the callback never raises')
end, '\n[lua]\nraw = true\n')

scenario('message', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local NOT_BOUND = 18
  local group_id = sim.groups['np!hGx1'].id
  local unit_id = sim.by_name['np.hGx2'].mission_id
  local r

  r = run({ message(3, 1, 5, 'g'), message(4, 2, 5, 'u'), message(1, 0, 5, 'all') }, 'message, no campaign', false)
  expect(r[1], NOT_BOUND, 'MESSAGE to a group with campaign 0')
  expect(r[2], NOT_BOUND, 'MESSAGE to a unit with campaign 0')
  expect(r[3], APPLIED, 'MESSAGE to all with campaign 0')
  assert(#sim.messages == 1, 'DCS got ' .. #sim.messages .. ' messages, not 1')
  sim.messages = {}

  r = run({
    message(1, 0, 10, 'all', true),
    message(2, 2, 0, 'side'),
    message(5, 80, 86400, 'country', true),
    message(3, 1, 2.5, 'group'),
    message(4, 2, 7, 'unit'),
    message(3, 99, 5, 'nogroup'),
    message(4, 99, 5, 'nounit'),
    message(0, 0, 5, 'noscope'),
    message(6, 0, 5, 'noscope'),
    message(2, 4294967296, 5, 'big'),
    message(1, 0, -1, 'short'),
    message(1, 0, 86400.5, 'long'),
    message(1, 0, 0 / 0, 'nan'),
    message(1, 0, math.huge, 'inf'),
  }, 'message scopes')
  for seq = 1, 5 do
    expect(r[seq], APPLIED, 'MESSAGE ' .. seq)
  end
  expect(r[6], NOT_BOUND, 'MESSAGE to no group')
  expect(r[7], NOT_BOUND, 'MESSAGE to no unit')
  for seq = 8, 14 do
    expect(r[seq], BAD_PAYLOAD, 'MESSAGE ' .. seq)
  end
  local want = {
    { 'outText', nil, 'all', 10, true },
    { 'outTextForCoalition', 2, 'side', 0, false },
    { 'outTextForCountry', 80, 'country', 86400, true },
    { 'outTextForGroup', group_id, 'group', 2.5, false },
    { 'outTextForUnit', unit_id, 'unit', 7, false },
  }
  assert(#sim.messages == #want, 'DCS got ' .. #sim.messages .. ' messages, not ' .. #want)
  for i, w in ipairs(want) do
    local m = sim.messages[i]
    assert(m.call == w[1] and m.target == w[2] and m.text == w[3] and m.seconds == w[4] and m.clear == w[5],
      'message ' .. i .. ' is ' .. m.call .. ' ' .. tostring(m.target) .. ' ' .. m.text .. ' ' .. m.seconds)
  end
  sim.message_raises = true
  r = run({ message(1, 0, 5, 'x') }, 'a message that raises')
  expect(r[1], 22, 'MESSAGE that raises')
  S.ok('MESSAGE: each scope calls its DCS form, a missing object or campaign 0 is NOT_BOUND, bad seconds and scopes are BAD_PAYLOAD')
end, '\n[lua]\nraw = true\n')

scenario('extension-eval', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local server = ctx.host.states.server
  local function eval(source)
    return with('EVAL', {}, PACK.lv(source))
  end
  local r
  local function set(name, rev, source)
    return { op = 'EXTENSION_SET', name = name, rev = rev, source = source }
  end
  local function remove(name)
    return with('EXTENSION_REMOVE', {}, PACK.str(name))
  end
  local function invoke(name)
    return { op = 'EXTENSION_INVOKE', name = name, args = {} }
  end

  r = run({
    set('x', 1, "return { invoke = function() return 'hi' end, on_stop = function() _G.ext_stopped = (_G.ext_stopped or 0) + 1 end }"),
    invoke('x'),
    remove('x'),
    invoke('x'),
    remove('x'),
    remove('never'),
  }, 'extension removal')
  expect(r[1], APPLIED, 'EXTENSION_SET')
  expect(r[2], APPLIED, 'EXTENSION_INVOKE')
  assert(r[2].value == 'hi', 'the extension answered ' .. tostring(r[2].value))
  expect(r[3], APPLIED, 'EXTENSION_REMOVE')
  assert(rawget(server, 'ext_stopped') == 1, 'on_stop ran ' .. tostring(rawget(server, 'ext_stopped')) .. ' times, not 1')
  expect(r[4], LUA_ERROR, 'EXTENSION_INVOKE after the removal')
  expect(r[5], APPLIED, 'EXTENSION_REMOVE of a removed extension')
  expect(r[6], APPLIED, 'EXTENSION_REMOVE of an unknown name')
  assert(rawget(server, 'ext_stopped') == 1, 'on_stop ran again: ' .. tostring(rawget(server, 'ext_stopped')))
  assert(next(server.__nanoprobe_shim.extensions) == nil, 'the shim holds an extension')

  r = run({
    eval('return 1 + 2'),
    eval('return 1, 2'),
    eval('local x = 1'),
    eval('eval_global = 5 return eval_global'),
    eval('trigger = 7 return type(trigger)'),
    eval('return type(trigger) .. type(coalition)'),
    eval('x = = 1'),
    eval("error('boom')"),
    eval("error({})"),
    eval('return nil'),
    eval(12),
  }, 'EVAL')
  expect(r[1], APPLIED, 'EVAL of a sum')
  assert(r[1].value == 3, 'EVAL returned ' .. tostring(r[1].value))
  expect(r[2], APPLIED, 'EVAL of two values')
  assert(r[2].value == 1, 'EVAL returned ' .. tostring(r[2].value) .. ' for two values')
  expect(r[3], APPLIED, 'EVAL of no value')
  expect(r[4], APPLIED, 'EVAL assigning a global')
  assert(r[4].value == 5, 'the global read back as ' .. tostring(r[4].value))
  assert(rawget(server, 'eval_global') == nil, 'EVAL set a global of the state')
  expect(r[5], APPLIED, 'EVAL assigning a name of DCS')
  assert(r[5].value == 'number', 'the assigned name read back as ' .. tostring(r[5].value))
  assert(type(server.trigger) == 'table', 'EVAL replaced a global of the state')
  expect(r[6], APPLIED, 'EVAL reading the state')
  assert(r[6].value == 'tabletable', 'a new chunk reads ' .. tostring(r[6].value) .. ', so an earlier one leaked')
  expect(r[7], LUA_ERROR, 'EVAL of source that does not compile')
  assert(r[7].message:find('eval', 1, true), 'the compile error is ' .. r[7].message)
  expect(r[8], LUA_ERROR, 'EVAL that raises')
  assert(r[8].message:find('boom', 1, true), 'the error is ' .. r[8].message)
  expect(r[9], LUA_ERROR, 'EVAL that raises a table')
  expect(r[10], APPLIED, 'EVAL of nil')
  expect(r[11], LUA_ERROR, 'EVAL of a source that is not a string')
  S.ok('EXTENSION_REMOVE runs on_stop once and forgets the extension; EVAL returns its first value in its own environment, LUA_ERROR for a compile or run error')
end, '\n[lua]\nraw = true\n')

scenario('chat', nil, function(ctx)
  local sim, run, expect = effect_world(ctx)
  local host = ctx.host
  local r
  -- No campaign: CHAT takes any.
  r = run({ chat(0, 'no campaign') }, 'chat with no campaign', false)
  expect(r[1], APPLIED, 'CHAT with campaign 0')
  assert(#host.chats == 1, 'the chat with campaign 0 did not reach DCS')
  host.chats = {}
  r = run({
    chat(0, 'everyone'),
    chat(7, 'one player'),
    chat(0, string.rep('a', 1024)),
    chat(0, ''),
    chat(0, string.rep('a', 1025)),
  }, 'chat')
  expect(r[1], APPLIED, 'CHAT to everyone')
  expect(r[2], APPLIED, 'CHAT to a player')
  expect(r[3], APPLIED, 'CHAT of 1,024 bytes')
  expect(r[4], BAD_PAYLOAD, 'CHAT of no text')
  expect(r[5], BAD_PAYLOAD, 'CHAT of 1,025 bytes')
  assert(#host.chats == 3, 'DCS got ' .. #host.chats .. ' chats, not 3')
  local a, b = host.chats[1], host.chats[2]
  assert(a.call == 'send_chat' and a.text == 'everyone' and a.all == true, 'the chat to everyone differs')
  assert(b.call == 'send_chat_to' and b.text == 'one player' and b.player == 7, 'the chat to a player differs')
  host.chat_raises = true
  r = run({ chat(0, 'raises') }, 'chat that raises')
  expect(r[1], 22, 'CHAT that raises')
  S.ok('CHAT: player 0 is send_chat, another player send_chat_to, any campaign, empty or long text is BAD_PAYLOAD')
end)

-- MISSION_LOAD: the hook answers, takes no further command, calls `finish`,
-- and only then `net.load_mission`. The hook is wrapped to record the order of
-- the agent's calls.
local ORDER_HOOK = S.read(root .. 'lua/agent/hook.lua'):gsub('    agent = result\n', [[
    agent = result
    do
      __order = {}
      local function note(name)
        local f = agent[name]
        agent[name] = function(...)
          local first, second, third = f(...)
          __order[#__order + 1] = name == 'command' and ('command ' .. tostring(first)) or name
          return first, second, third
        end
      end
      note('command')
      note('command_done')
      note('finish')
      local load = net.load_mission
      net.load_mission = function(path)
        __order[#__order + 1] = 'load ' .. path
        return load(path)
      end
    end
]], 1)
scenario('mission-load', nil, function(ctx)
  local sim, gen = hook_world(ctx)
  local host = ctx.host
  local hook = host.states.hook
  local function sequence()
    return table.concat(hook.__order, ';')
  end
  ctx.script(json({ campaign = 7, commands = {
    { op = 'MISSION_LOAD', gen = gen, payload_hex = hexof(PACK.str('Missions/next.miz')) },
    chat(0, 'after the load'),
  } }))
  local frame_no, load_frame, chat_frame = 0, nil, nil
  assert(ctx.run(30, function()
    return ctx.ended() ~= nil
  end, function()
    frame_no = frame_no + 1
    host:frame(0.01)
    load_frame = load_frame or (#host.loads > 0 and frame_no or nil)
    chat_frame = chat_frame or (#host.chats > 0 and frame_no or nil)
  end), 'no end line:\n' .. dump(ctx))
  assert(ctx.result(1).code == APPLIED, 'MISSION_LOAD answered ' .. tostring(ctx.result(1).code_name) .. ':\n' .. dump(ctx))
  assert(ctx.result(2).code == APPLIED, 'the CHAT after it answered ' .. tostring(ctx.result(2).code_name))
  assert(host.loads[1] == 'Missions/next.miz', 'DCS was asked to load ' .. tostring(host.loads[1]))
  assert(chat_frame and load_frame and chat_frame > load_frame, 'the chat ran in frame ' .. tostring(chat_frame)
    .. ', not after the load in frame ' .. tostring(load_frame))
  -- The agent's calls in the frame of the load: the command, its answer,
  -- `finish`, then the load, with no command taken between.
  local at = sequence():find('command 56;command_done;finish;load Missions/next.miz', 1, true)
  assert(at, 'the calls were ' .. sequence())
  local log = S.read(ctx.profile .. 'nanoprobe/logs/hook.log') or ''
  assert(log:find('mission load requested', 1, true), 'the hook did not log the load:\n' .. log)
  S.ok('MISSION_LOAD: APPLIED, no further command in the frame, net.load_mission after finish, the next command runs after the load')
end, nil, ORDER_HOOK)

-- Players and moderation (S16, spec/commands.md, "Moderation" and "Player and
-- ban queries"; spec/records.md, "Layout: PLAYER"). The callbacks, the hook's
-- handlers for MODERATE, PLAYERS and BANS, and the PLAYER records the
-- sidecar reads, with the mock's players and ban list.
local function moderate(action, player, side, seconds, ucid, slot, reason)
  return with('MODERATE', {}, PACK.u32(action) .. PACK.u32(player) .. PACK.u32(side) .. PACK.u32(0)
    .. PACK.f64(seconds) .. PACK.str(ucid) .. PACK.str(slot) .. PACK.str(reason))
end
local KICK, BAN, UNBAN, SLOT = 1, 2, 3, 4

-- The records of one kind the sidecar printed, in order.
local function printed(ctx, kind)
  local out = {}
  for _, e in ipairs(ctx.events()) do
    if e.ev == kind then
      out[#out + 1] = e
    end
  end
  return out
end

scenario('players', nil, function(ctx)
  local sim, run, expect = effect_world(ctx)
  local host = ctx.host
  -- The server's own connection, and a player DCS gives no ucid, write
  -- nothing. A seat is the `_N` suffix of the slot minus one.
  host:callback('onPlayerConnect', 1)
  host:connect(2, { ucid = 'ucid-2', name = 'Alpha', side = 2, slot = 'slot-7' })
  host:connect(3, { ucid = 'ucid-3', name = 'Bravo', side = 1, slot = 'pilot_2' })
  host.players[4] = { name = 'No ucid' }
  host:callback('onPlayerConnect', 4)
  host:change_slot(2, 1, 'slot-9')
  host:change_slot(3, 0, '')
  host:callback('onPlayerChangeSlot', 1)
  local returned = host:chat(2, 'hello there')
  host:callback('onPlayerTrySendChat', 1, 'the server speaks', true)
  host.players[4] = nil
  -- The mock removes the player before the callback: the hook answers a
  -- DISCONNECT from what it last held.
  host:disconnect(3, 7)
  host:disconnect(9, 1)
  for _, v in ipairs(returned) do
    assert(#v == 0, 'onPlayerTrySendChat returned a value')
  end
  assert(#returned == 1, 'onPlayerTrySendChat is registered ' .. #returned .. ' times')
  for _, name in ipairs({ 'onPlayerConnect', 'onPlayerDisconnect', 'onPlayerChangeSlot' }) do
    for _, v in ipairs(host:callback_values(name, 1, 0)) do
      assert(#v == 0, name .. ' returned a value')
    end
  end

  local r = run({ { op = 'PLAYERS' } }, 'players')
  expect(r[1], APPLIED, 'PLAYERS')
  local got = printed(ctx, 'player')
  local function same(i, want)
    local p = got[i]
    assert(p, 'no PLAYER record ' .. i .. ':\n' .. dump(ctx))
    for k, v in pairs(want) do
      assert(p[k] == v, 'PLAYER ' .. i .. ' ' .. k .. ' is ' .. tostring(p[k]) .. ', not ' .. tostring(v))
    end
  end
  assert(#got == 6, #got .. ' PLAYER records, not 6:\n' .. dump(ctx))
  same(1, { event = 1, id = 2, side = 2, seat = 0, client = 'ucid-2', name = 'Alpha', slot = 'slot-7', text = '' })
  same(2, { event = 1, id = 3, side = 1, seat = 1, client = 'ucid-3', name = 'Bravo', slot = 'pilot_2', text = '' })
  same(3, { event = 3, id = 2, side = 1, seat = 0, slot = 'slot-9' })
  same(4, { event = 3, id = 3, side = 0, seat = 0, slot = '' })
  same(5, { event = 4, id = 2, side = 1, client = 'ucid-2', name = 'Alpha', slot = 'slot-9', text = 'hello there' })
  same(6, { event = 2, id = 3, client = 'ucid-3', name = 'Bravo', text = '7' })
  S.ok('player callbacks write CONNECT, SLOT, CHAT and DISCONNECT records, skip the server and a player with no ucid, and return nothing')

  -- PLAYERS: every connected player but the server.
  assert(#r[1].value == 1, 'PLAYERS listed ' .. #r[1].value .. ' players')
  local p = r[1].value[1]
  assert(p.id == 2 and p.ucid == 'ucid-2' and p.name == 'Alpha' and p.side == 1 and p.slot == 'slot-9',
    'PLAYERS gave the wrong fields')
  host:connect(5, { ucid = 'ucid-5', name = 'Echo', side = 2, slot = 'e_1' })
  r = run({ { op = 'PLAYERS' } }, 'players again')
  expect(r[1], APPLIED, 'PLAYERS again')
  assert(#r[1].value == 2 and r[1].value[1].id == 2 and r[1].value[2].id == 5, 'PLAYERS did not list both in order')
  -- A slot DCS gives as a number is decimal text, and a missing one is ''.
  host:connect(6, { ucid = 'ucid-6', name = 'Foxtrot', side = 2, slot = 12 })
  host:connect(7, { ucid = 'ucid-7', name = 'Golf', side = 1 })
  r = run({ { op = 'PLAYERS' } }, 'players with numeric slots')
  expect(r[1], APPLIED, 'PLAYERS with numeric slots')
  assert(r[1].value[3].slot == '12' and r[1].value[4].slot == '', 'PLAYERS did not give a slot as text')

  -- BANS: the four fields only; an address is left out.
  host.bans = {
    { ucid = 'ban-1', name = 'Old', banned_until = 1234, reason = 'rude', ipaddr = '1.2.3.4' },
    { ucid = 'ban-2', banned_until = 99, extra = { 1 } },
  }
  r = run({ { op = 'BANS' } }, 'bans')
  expect(r[1], APPLIED, 'BANS')
  local list = r[1].value
  assert(#list == 2, 'BANS listed ' .. #list)
  assert(list[1].ucid == 'ban-1' and list[1].name == 'Old' and list[1].banned_until == 1234
    and list[1].reason == 'rude' and list[1].ipaddr == nil, 'the first ban differs')
  assert(list[2].ucid == 'ban-2' and list[2].banned_until == 99 and list[2].extra == nil, 'the second ban differs')
  host.net_raises.banlist_get = true
  r = run({ { op = 'BANS' } }, 'bans that raise')
  expect(r[1], 22, 'BANS that raises')
  S.ok('PLAYERS lists every player but the server with five fields, BANS keeps four fields and leaves out an address')
end)

scenario('moderate', nil, function(ctx)
  local sim, run, expect = effect_world(ctx)
  local host = ctx.host
  local NOT_BOUND = 18
  host:connect(2, { ucid = 'ucid-2', name = 'Alpha', side = 2, slot = 'slot-7' })
  host:connect(3, { ucid = 'ucid-3', name = 'Bravo', side = 1, slot = 'slot-8' })
  host:connect(4, { ucid = 'ucid-4', name = 'Charlie', side = 1, slot = 'slot-9' })
  local function calls()
    local out = {}
    for _, c in ipairs(host.moderation) do
      local args = {}
      for i = 1, 3 do
        args[i] = tostring(c.args and c.args[i])
      end
      out[#out + 1] = c.call .. '(' .. table.concat(args, ',') .. ')'
    end
    return table.concat(out, ';')
  end
  local r = run({
    moderate(KICK, 2, 0, 0, 'ucid-2', '', 'bye'),
    moderate(KICK, 3, 0, 0, 'ucid-X', '', 'wrong ucid'),
    moderate(KICK, 9, 0, 0, 'ucid-9', '', 'not connected'),
    moderate(BAN, 3, 0, 60, 'ucid-3', '', 'rule 1'),
    moderate(BAN, 0, 0, 0, 'ucid-off', '', 'by ucid'),
    moderate(BAN, 4, 0, 0, 'ucid-X', '', 'wrong ucid'),
    moderate(UNBAN, 0, 0, 0, 'ucid-off', '', ''),
    moderate(SLOT, 4, 2, 0, 'ucid-4', 'blue-1', ''),
    moderate(SLOT, 4, 0, 0, 'ucid-4', '', ''),
    moderate(SLOT, 3, 2, 0, 'ucid-X', 'blue-1', ''),
  }, 'moderation')
  expect(r[1], APPLIED, 'KICK')
  expect(r[2], NOT_BOUND, 'KICK with another ucid')
  expect(r[3], NOT_BOUND, 'KICK of nobody')
  expect(r[4], APPLIED, 'BAN with a player')
  expect(r[5], APPLIED, 'BAN by ucid')
  expect(r[6], NOT_BOUND, 'BAN with another ucid')
  expect(r[7], APPLIED, 'UNBAN')
  expect(r[8], APPLIED, 'SLOT')
  expect(r[9], APPLIED, 'SLOT to spectators')
  expect(r[10], NOT_BOUND, 'SLOT with another ucid')
  assert(calls() == 'kick(2,bye,nil);banlist_add(3,60,rule 1);banlist_add_by_ucid(ucid-off,0,by ucid);'
    .. 'banlist_remove(ucid-off,nil,nil);force_player_slot(4,2,blue-1);force_player_slot(4,0,)',
    'DCS got ' .. calls())
  assert(host.players[2] == nil and host.players[3] ~= nil, 'the kick took the wrong player')
  assert(host.players[4].side == 0 and host.players[4].slot == '', 'the slot did not move')
  assert(#host.bans == 1 and host.bans[1].ucid == 'ucid-3', 'the ban list is wrong')

  -- What the agent refuses: no handler runs.
  host.moderation = {}
  r = run({
    moderate(KICK, 1, 0, 0, 'server-ucid', '', 'the server'),
    moderate(0, 3, 0, 0, 'ucid-3', '', ''),
    moderate(5, 3, 0, 0, 'ucid-3', '', ''),
    moderate(KICK, 3, 0, 0, '', '', ''),
    moderate(BAN, 3, 0, -1, 'ucid-3', '', ''),
    moderate(BAN, 3, 0, 0 / 0, 'ucid-3', '', ''),
    moderate(BAN, 3, 0, math.huge, 'ucid-3', '', ''),
  }, 'refused moderation')
  for seq = 1, 7 do
    expect(r[seq], BAD_PAYLOAD, 'MODERATE ' .. seq)
  end
  assert(#host.moderation == 0, 'a refused MODERATE reached DCS')

  -- A call that returns false or raises: DCS_REFUSED, and no ucid in the message.
  host.net_returns.kick = false
  host.net_raises.banlist_remove = 'boom for ucid-3 and ucid-3 again'
  host.net_raises.force_player_slot = true
  r = run({
    moderate(KICK, 3, 0, 0, 'ucid-3', '', 'x'),
    moderate(UNBAN, 0, 0, 0, 'ucid-3', '', ''),
    moderate(SLOT, 3, 1, 0, 'ucid-3', 's', ''),
  }, 'moderation that fails')
  for seq = 1, 3 do
    expect(r[seq], 22, 'MODERATE that fails ' .. seq)
  end
  assert(r[1].message == 'DCS returned false', 'the message is ' .. r[1].message)
  assert(r[2].message:find('boom for', 1, true) and r[2].message:find('<ucid>', 1, true), 'the message is ' .. r[2].message)
  for _, e in ipairs(ctx.events()) do
    if e.ev == 'result' then
      assert(not e.message:find('ucid-', 1, true), 'a ucid is in a RESULT message: ' .. e.message)
    end
  end
  for _, f in ipairs({ 'nanoprobe/logs/hook.log', 'nanoprobe/logs/agent.log' }) do
    assert(not (S.read(ctx.profile .. f) or ''):find('ucid%-%d'), 'a ucid is in ' .. f)
  end
  S.ok('MODERATE: KICK, BAN, UNBAN and SLOT call DCS, a missing or changed ucid is NOT_BOUND, a bad payload is refused, a false return or a raise is DCS_REFUSED, and no ucid is in a message')
end)

-- emit (spec/commands.md, "emit"): the limits and the reasons, with EVAL as
-- the caller, and the AUTHOR_EVENT records the sidecar reads.
scenario('emit', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local server = ctx.host.states.server
  local function eval(source)
    return with('EVAL', {}, PACK.lv(source))
  end
  -- Each case returns `{ok, reason}` from one emit.
  local function try(body)
    if not body:find('^local ') then
      body = 'return ' .. body
    end
    return eval('local function f() ' .. body .. ' end local ok, why = f() return { ok, tostring(why) }')
  end
  local deep = function(n)
    return 'local t = {} local top = t for i = 2, ' .. n .. ' do t[1] = {} t = t[1] end'
  end
  local cases = {
    { try("emit('first', { x = 1, text = 'a', list = { true, false } })"), true },
    { try("emit('', 1)"), 'name' },
    { try("emit(" .. "'" .. string.rep('n', 129) .. "', 1)"), 'name' },
    { try("emit('" .. string.rep('n', 128) .. "', 1)"), true },
    { try('emit(nil, 1)'), 'name' },
    { try('emit(7, 1)'), 'name' },
    { try("emit('nil payload')"), true },
    { try("emit('fn', { f = function() end, c = coroutine.create(function() end), p = print })"), true },
    -- 16 KiB encoded: a string is 5 bytes of header and its text.
    { try("emit('big', string.rep('x', 16379))"), true },
    { try("emit('big', string.rep('x', 16380))"), 'too_large' },
    { try("emit('big', { string.rep('x', 9000), string.rep('y', 9000) })"), 'too_large' },
    { try(deep(16) .. ' return emit("deep", top)'), true },
    { try(deep(17) .. ' return emit("deep", top)'), 'too_deep' },
    { try("local t = {} t.self = t return emit('cycle', t)"), 'too_deep' },
    { try("local a, b = {}, {} a.b = b b.a = a return emit('cycle', a)"), 'too_deep' },
    { try("emit('objects', 1, { a = 1 })"), 'objects' },
    { try("emit('objects', 1, 5)"), 'objects' },
    { try("emit('objects', 1, { 424242 })"), 'objects' },
    { try("emit('objects', 1, { {} })"), 'objects' },
    { try("emit('objects', 1, { 'x' })"), 'objects' },
    { try("emit('objects', 1, { [2] = 424242 })"), 'objects' },
    { try("local t = {} for i = 1, 17 do t[i] = 424242 end return emit('objects', 1, t)"), 'objects' },
    -- Reads are raw: a metamethod runs nothing, and nothing raises.
    { try("emit('meta', setmetatable({ a = 1 }, { __index = function() error('read') end, __len = function() error('len') end }))"), true },
    { try("emit('meta', 1, setmetatable({}, { __index = function() error('read') end }))"), true },
    { try("emit('keys', { [print] = 1, [{}] = 2, ok = 3 })"), true },
  }
  local commands = {}
  for i, c in ipairs(cases) do
    commands[i] = c[1]
  end
  local r = run(commands, 'emit')
  for i, c in ipairs(cases) do
    expect(r[i], APPLIED, 'EVAL of case ' .. i)
    local v = r[i].value
    if c[2] == true then
      assert(v[1] == true, 'case ' .. i .. ' answered ' .. tostring(v[1]) .. ' ' .. tostring(v[2]))
    else
      assert(v[1] == false and v[2] == c[2], 'case ' .. i .. ' answered ' .. tostring(v[1]) .. ' ' .. tostring(v[2])
        .. ', not ' .. c[2])
    end
  end
  -- The records of the accepted ones, in order.
  local got = printed(ctx, 'author')
  local names = {}
  for _, e in ipairs(got) do
    names[#names + 1] = #e.name > 20 and ('n' .. #e.name) or e.name
  end
  assert(table.concat(names, ',') == 'first,n128,nil payload,fn,big,deep,meta,meta,keys',
    'the AUTHOR_EVENT records are ' .. table.concat(names, ','))
  local first = got[1].payload
  assert(first.x == 1 and first.text == 'a' and first.list[1] == true and first.list[2] == false,
    'the first payload differs')
  assert(got[3].payload == S.null, 'a nil payload came back ' .. tostring(got[3].payload))
  local fn = got[4].payload
  assert(fn.f == '<function>' and fn.c == '<thread>' and fn.p == '<function>', 'a function became ' .. tostring(fn.f))
  assert(got[7].payload.a == 1 and got[8].payload == 1, 'the metatable payload differs')
  assert(got[9].payload.ok == 3, 'the keys payload differs')
  assert(#got[1].objects == 0, 'an event with no objects holds some')
  S.ok('emit: names, 16 KiB, 16 levels, cycles and object arrays are checked with their reasons, reads are raw, and each accepted emit is an AUTHOR_EVENT')
end, '\n[lua]\nraw = true\n')

scenario('emit-objects', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  local sim, run, expect = effect_world(ctx)
  local server = ctx.host.states.server
  local function eval(source)
    return with('EVAL', {}, PACK.lv(source))
  end
  -- The sidecar enumerates, and the agent's table holds the unit.
  ctx.script(json({ campaign = 7, commands = {
    { op = 'ENUMERATE' },
    { op = 'EVAL', wait_ms = 1500, payload_hex = hexof(PACK.lv(
      "local u = Unit.getByName('np.hGx2') local ok, why = emit('with unit', { n = 1 }, { u }) "
      .. "local ok2, why2 = emit('with id', 2, { u.id_ }) "
      .. "local ok3, why3 = emit('missing', 2, { u, 99 }) "
      .. "return { ok, tostring(why), ok2, tostring(why2), ok3, tostring(why3), u.id_ }")) },
  } }))
  finish(ctx, 'emit with units')
  local v = ctx.result(1).value
  assert(v[1] == true and v[3] == true, 'emit with a unit answered ' .. tostring(v[1]) .. ' ' .. tostring(v[2])
    .. ' ' .. tostring(v[3]) .. ' ' .. tostring(v[4]) .. ':\n' .. dump(ctx))
  assert(v[5] == false and v[6] == 'objects', 'emit with an unknown unit answered ' .. tostring(v[5]) .. ' ' .. tostring(v[6]))
  local got = printed(ctx, 'author')
  assert(#got == 2, #got .. ' AUTHOR_EVENT records, not 2:\n' .. dump(ctx))
  for _, e in ipairs(got) do
    assert(#e.objects == 1 and e.objects[1] == v[7], 'the objects are ' .. table.concat(e.objects, ','))
  end
  S.ok('emit: a unit object or its id_ names the unit, and the record holds its object ID')
end, '\n[lua]\nraw = true\n')

scenario('emit-off', S.read(root .. 'lua/agent/shim.lua'), function(ctx)
  ctx.host:load_mission('first', { paused = true })
  ctx.run(0.5)
  local server = ctx.host.states.server
  local shim = server.__nanoprobe_shim
  assert(rawget(server, 'emit') == nil, 'emit is a global with raw Lua off')
  local ok, why = shim.emit('x', 1)
  assert(ok == false and why == 'disabled', 'emit answered ' .. tostring(ok) .. ' ' .. tostring(why))
  ok, why = shim.emit(nil, 1)
  assert(ok == false and why == 'disabled', 'emit with raw Lua off answered ' .. tostring(why))
  S.ok('emit is no global with raw Lua off, and the shim function answers false and disabled')
end)

-- Extension hooks (S16, spec/commands.md, "Extensions"): on_event, on_frame,
-- the errors in a row, the budget and the turn order, the replaced globals,
-- and on_stop on a removal, a campaign switch and a simulation stop. The
-- mission runs, so on_frame is called; the frames are the mock's.
local SHIM = S.read(root .. 'lua/agent/shim.lua')

-- A script of commands, with the frames stepped by `step(frame_no)` until it
-- ends.
local function play(ctx, commands, step, what)
  ctx.stop()
  for _, c in ipairs(commands) do
    c.gen = c.gen or 1
  end
  ctx.script(json({ campaign = 7, commands = commands }))
  local n = 0
  assert(ctx.run(40, function()
    return ctx.ended() ~= nil
  end, function()
    n = n + 1
    if step then
      step(n)
    end
    ctx.host:frame(0.01)
  end), what .. ': no end line:\n' .. dump(ctx))
  assert(ctx.ended().complete, what .. ': a command got no RESULT:\n' .. dump(ctx))
end

local function ext_set(name, rev, source)
  return { op = 'EXTENSION_SET', name = name, rev = rev, source = source }
end

local function ext_invoke(name, wait_ms)
  return { op = 'EXTENSION_INVOKE', name = name, args = {}, wait_ms = wait_ms }
end

-- A running mission: the world, and the pause taken off.
local function running_world(ctx)
  local sim = hook_world(ctx)
  ctx.host:set_pause(false, { silent = true })
  return sim
end

scenario('extension-hooks', SHIM, function(ctx)
  running_world(ctx)
  local host = ctx.host
  local server = host.states.server
  local shim = server.__nanoprobe_shim
  local shot = server.world.event.S_EVENT_SHOT

  -- on_event gets a shallow copy of every event, and on_frame is called each
  -- frame while the mission runs.
  local source = [[
    local frames, events, last = 0, 0, nil
    return {
      on_event = function(event) events = events + 1 last = event end,
      on_frame = function(t) frames = frames + 1 end,
      invoke = function() return { frames = frames, events = events, id = last and last.id,
        text = last and last.text, time = last and last.time } end,
    }
  ]]
  local fired, source_event = 0, nil
  play(ctx, { ext_set('f', 1, source), ext_invoke('f', 800), ext_invoke('f', 0) }, function()
    if shim.extensions.f and fired < 5 then
      fired = fired + 1
      source_event = { id = shot, time = fired, text = 'shot ' .. fired, extra = { 1 } }
      host:event(source_event)
    end
  end, 'on_event and on_frame')
  local v = ctx.result(3).value
  assert(v.events == 5 and v.id == shot and v.text == 'shot 5' and v.time == 5, 'on_event saw ' .. json(v))
  assert(v.frames >= 20, 'on_frame ran ' .. tostring(v.frames) .. ' times in 0.8 s of frames')
  local e = shim.extensions.f
  assert(e.calls >= 25 and e.skipped == 0 and e.errors == 0 and not e.stopped, 'the counters differ')
  S.ok('on_event gets each event once, with its fields copied, and on_frame runs every frame of a running mission')

  -- A paused mission: on_event still runs, on_frame does not.
  host:set_pause(true, { silent = true })
  local before = ctx.result(3).value.frames
  fired = 0
  play(ctx, { ext_invoke('f', 0), ext_invoke('f', 500), ext_invoke('f', 0) }, function()
    if fired < 2 then
      fired = fired + 1
      host:event({ id = shot, time = 100 + fired, text = 'paused' })
    end
  end, 'a paused mission')
  local a, b = ctx.result(1).value, ctx.result(3).value
  assert(b.frames == a.frames, 'on_frame ran while paused: ' .. a.frames .. ' then ' .. b.frames)
  assert(b.events == 5 + 2 and b.text == 'paused', 'on_event ran ' .. tostring(b.events) .. ' times')
  host:set_pause(false, { silent = true })
  S.ok('while the mission is paused on_event runs and on_frame does not')

  -- After the unpause the first pump in RUNNING already asks for the next,
  -- so on_frame runs on the frames that follow without a sampling pump.
  host:set_pause(false, { silent = true })
  local frames_at = {}
  play(ctx, { ext_invoke('f', 0), ext_invoke('f', 200), ext_invoke('f', 0) }, nil, 'after the unpause')
  frames_at.first, frames_at.last = ctx.result(1).value.frames, ctx.result(3).value.frames
  assert(frames_at.last - frames_at.first >= 10,
    'on_frame ran ' .. (frames_at.last - frames_at.first) .. ' times in 20 frames after the unpause')
  S.ok('on_frame runs on every frame right after an unpause')

  -- The queue holds 256 events: the oldest are dropped and counted.
  fired = 0
  play(ctx, { ext_invoke('f', 0), ext_invoke('f', 300), ext_invoke('f', 0) }, function(n)
    if n == 3 then
      for i = 1, 300 do
        host:event({ id = shot, time = i, text = 'burst ' .. i })
      end
    end
  end, 'a burst of events')
  local c = ctx.result(3).value
  assert(shim.extensions.f.skipped == 44, 'the queue dropped ' .. tostring(shim.extensions.f.skipped) .. ', not 44')
  assert(c.events == 7 + 256 and c.text == 'burst 300', 'on_event saw ' .. json(c))
  S.ok('an extension queue of 256 events drops the oldest and counts them')
end, '\n[lua]\nraw = true\n')

scenario('extension-errors', SHIM, function(ctx)
  running_world(ctx)
  local host = ctx.host
  local server = host.states.server
  local shim = server.__nanoprobe_shim
  local bad = "return { on_frame = function() error('oops') end, invoke = function() return 'bad' end }"
  -- Nine errors and a success never reach ten in a row.
  local flaky = "local n = 0 return { on_frame = function() n = n + 1 if n % 10 ~= 0 then error('flaky') end end }"
  play(ctx, { ext_set('bad', 1, bad), ext_set('flaky', 1, flaky), ext_invoke('bad', 600), ext_invoke('bad', 0) },
    nil, 'hooks that raise')
  local e = shim.extensions.bad
  assert(e.stopped and e.errors >= 10 and e.calls == 10, 'the bad extension: stopped ' .. tostring(e.stopped)
    .. ', ' .. e.errors .. ' errors, ' .. e.calls .. ' calls')
  assert(not shim.extensions.flaky.stopped, 'nine errors and a success stopped the flaky extension')
  -- The stopped extension answers no hook.
  assert(ctx.result(3).code == LUA_ERROR and ctx.result(4).code == LUA_ERROR, 'a stopped extension answered an invoke')
  local log = S.read(ctx.profile .. 'nanoprobe/logs/shim.log') or ''
  assert(log:find('extension hooks stopped after errors', 1, true), 'the stop is not in the log:\n' .. log)
  local stopped
  for _, r in ipairs(printed(ctx, 'extension')) do
    if r.name == 'bad' and r.stopped == 1 then
      stopped = r
    end
  end
  assert(stopped and stopped.rev == 1 and stopped.calls == 10, 'no EXTENSION record says the extension stopped:\n' .. dump(ctx))
  -- A newer revision starts it again.
  play(ctx, { ext_set('bad', 2, "return { invoke = function() return 'fixed' end }"), ext_invoke('bad', 0) }, nil, 'a new revision')
  assert(ctx.result(1).code == APPLIED and ctx.result(2).value == 'fixed', 'the new revision did not run')
  assert(not shim.extensions.bad.stopped and shim.extensions.bad.errors == 0, 'the new revision starts stopped')
  S.ok('ten errors in a row stop an extension with a log record and an EXTENSION record, nine and a success do not, and a newer revision starts it again')
end, '\n[lua]\nraw = true\n')

scenario('extension-budget', SHIM, function(ctx)
  running_world(ctx)
  local host = ctx.host
  local server = host.states.server
  local shim = server.__nanoprobe_shim
  local shot = server.world.event.S_EVENT_SHOT
  -- Each event takes longer than the whole budget (1 ms): one call a frame.
  local slow = [[
    local n = 0
    return {
      on_event = function() n = n + 1 local x = 0 for i = 1, 3000000 do x = x + i end end,
      invoke = function() return n end,
    }
  ]]
  local fast = "local n = 0 return { on_event = function() n = n + 1 end, on_frame = function() n = n + 1000 end }"
  local frames_at_burst, early
  play(ctx, { ext_set('a', 1, slow), ext_set('b', 1, slow), ext_invoke('a', 400) }, function(n)
    if shim.extensions.a and shim.extensions.b and not frames_at_burst then
      frames_at_burst = n
      for i = 1, 10 do
        host:event({ id = shot, time = i })
      end
    elseif frames_at_burst and n == frames_at_burst + 5 then
      -- Five frames after the burst: one call a frame at most, 20 queued.
      early = { shim.extensions.a.calls, shim.extensions.b.calls }
    end
  end, 'slow extensions')
  local a, b = shim.extensions.a, shim.extensions.b
  assert(early and early[1] > 0 and early[2] > 0, 'an extension never got its turn')
  assert(early[1] + early[2] <= 5, 'the budget let ' .. (early[1] + early[2]) .. ' calls through in 5 frames')
  assert(math.abs(early[1] - early[2]) <= 1, 'the turns are uneven: ' .. early[1] .. ' and ' .. early[2])
  assert(a.calls >= 10 and b.calls >= 10, 'the queued events were not all run later')
  assert(a.max_us > 1000 and a.total_us > 1000, 'the hook time is not counted: ' .. a.max_us)
  S.ok('hooks run only while the frame budget has time, and extensions take turns to go first')
end, '\n[lua]\nraw = true\n[budget]\ncommanded_ms = 1.0\n')

scenario('extension-records', SHIM, function(ctx)
  running_world(ctx)
  local shim = ctx.host.states.server.__nanoprobe_shim
  play(ctx, { ext_set('r', 4, "return { on_frame = function() end }"), ext_invoke('r', 10500) }, nil, 'EXTENSION records')
  local mine = {}
  for _, r in ipairs(printed(ctx, 'extension')) do
    if r.name == 'r' then
      mine[#mine + 1] = r
    end
  end
  assert(#mine >= 2 and #mine <= 3, #mine .. ' EXTENSION records in about 10 s:\n' .. dump(ctx))
  assert(mine[1].rev == 4 and mine[1].stopped == 0, 'the record differs')
  assert(mine[#mine].calls > mine[1].calls and mine[#mine].total_us >= mine[1].total_us, 'the counters did not grow')
  S.ok('the agent writes an EXTENSION record for each extension every 10 s, with its applied revision')
end, '\n[lua]\nraw = true\n')

scenario('extension-env', SHIM, function(ctx)
  local sim = running_world(ctx)
  local host = ctx.host
  local server = host.states.server
  local shim = server.__nanoprobe_shim
  local source = [[
    local r = { picked = 0, timer_calls = 0, args = '' }
    r.add_ok, r.add_err = pcall(world.addEventHandler, {})
    r.remove_ok, r.remove_err = pcall(world.removeEventHandler, {})
    r.reads = type(world.getAirbases) .. type(timer.getTime) .. type(missionCommands.addCommand)
    r.timer_id = timer.scheduleFunction(function(arg, t)
      r.timer_calls = r.timer_calls + 1
      r.timer_arg = arg
      if r.timer_calls < 3 then return t + 0.5 end
    end, 'tick', timer.getTime() + 0.5)
    local gone = timer.scheduleFunction(function() r.removed_ran = true end, nil, timer.getTime() + 0.5)
    timer.removeFunction(gone)
    local moved = timer.scheduleFunction(function() r.moved_ran = timer.getTime() end, nil, timer.getTime() + 100)
    timer.setFunctionTime(moved, timer.getTime() + 0.6)
    local sub = missionCommands.addSubMenu('Ext')
    missionCommands.addCommand('Go', sub, function(arg) r.picked = r.picked + 1 r.args = r.args .. arg end, 'x')
    local own = missionCommands.addSubMenu('Own')
    missionCommands.removeItem(own)
    r.coalition = missionCommands.addCommandForCoalition(2, 'Side', nil, function() end)
    return { invoke = function() return r end, on_stop = function() _G.env_stopped = true end }
  ]]
  local advanced = 0
  play(ctx, { ext_set('e', 1, source), ext_invoke('e', 0) }, nil, 'the extension environment')
  local r = ctx.result(2).value
  assert(r.add_ok == false and r.add_err:find('on_event', 1, true), 'addEventHandler answered ' .. tostring(r.add_err))
  assert(r.remove_ok == false and r.remove_err:find('on_event', 1, true), 'removeEventHandler answered ' .. tostring(r.remove_err))
  assert(r.reads == 'functionfunctionfunction', 'the globals read through as ' .. tostring(r.reads))
  assert(type(r.timer_id) == 'number', 'scheduleFunction returned ' .. tostring(r.timer_id))
  -- The timers are the agent's, not DCS's, and the items are in DCS.
  assert(host:timer_count() == 0, 'an extension timer reached DCS: ' .. host:timer_count())
  assert(joined(sim:menu_list('all')) == 'Ext,Ext/Go', 'the menu is ' .. joined(sim:menu_list('all')))
  assert(joined(sim:menu_list('c2')) == 'Side', 'the coalition menu is ' .. joined(sim:menu_list('c2')))

  -- Model time passes: the timer runs from the pump as a hook, three times,
  -- then stops; the moved one ran once; the removed one never.
  play(ctx, { ext_invoke('e', 0), ext_invoke('e', 0) }, function(n)
    host:frame(0.2)
    if n == 5 then
      sim:menu_select('all', { 'Ext', 'Go' })
      sim:menu_select('all', { 'Ext', 'Go' })
    end
  end, 'model time passes')
  play(ctx, { ext_invoke('e', 1200) }, function()
    host:frame(0.2)
  end, 'more time')
  r = ctx.result(1).value
  assert(r.timer_calls == 3 and r.timer_arg == 'tick', 'the timer ran ' .. tostring(r.timer_calls) .. ' times')
  assert(r.removed_ran == nil and r.moved_ran ~= nil, 'a removed timer ran, or a moved one did not')
  assert(r.picked == 2 and r.args == 'xx', 'the menu command ran ' .. tostring(r.picked) .. ' times with ' .. tostring(r.args))
  assert(host:timer_count() == 0, 'a timer reached DCS')
  assert(shim.extensions.e.calls >= 7, 'the hooks are not counted: ' .. shim.extensions.e.calls)

  -- EXTENSION_REMOVE: on_stop, and the items go from DCS.
  play(ctx, { with('EXTENSION_REMOVE', {}, PACK.str('e')) }, nil, 'removal')
  assert(rawget(server, 'env_stopped') == true, 'on_stop did not run')
  assert(#sim:menu_list('all') == 0 and #sim:menu_list('c2') == 0,
    'the extension left menu items: ' .. joined(sim:menu_list('all')))
  S.ok('an extension cannot add event handlers, its timers and menu commands run from the pump as hooks, and its menu items go with it')
end, '\n[lua]\nraw = true\n')

scenario('extension-stop', SHIM, function(ctx)
  local sim = running_world(ctx)
  local host = ctx.host
  local server = host.states.server
  local shim = server.__nanoprobe_shim
  local stopper = "return { invoke = function() return 1 end, on_stop = function() _G.stops = (_G.stops or 0) + 1 end }"
  -- A replacement runs on_stop of the old one; a failed one leaves it.
  play(ctx, {
    ext_set('s', 1, stopper),
    ext_set('s', 2, stopper),
    ext_set('s', 3, 'return {'),
    ext_set('s', 4, "error('chunk')"),
    ext_invoke('s'),
    mark(10, 1, 1, 0, 'a mark'),
    menu(30, 1, 0, 1, 0, true, 'Held'),
    ext_set('other', 1, stopper),
  }, nil, 'replacement')
  assert(ctx.result(2).code == APPLIED and ctx.result(3).code == LUA_ERROR and ctx.result(4).code == LUA_ERROR,
    'the replacements answered ' .. ctx.result(2).code_name .. ' ' .. ctx.result(3).code_name)
  assert(rawget(server, 'stops') == 1, 'on_stop ran ' .. tostring(rawget(server, 'stops')) .. ' times for a replacement')
  assert(ctx.result(5).code == APPLIED, 'the old extension stopped running after a failed replacement')
  assert(count(sim.marks) == 1 and joined(sim:menu_list('all')) == 'Held', 'the mark and the menu were not set')

  -- A campaign switch: on_stop of each, then gone, and the held marks,
  -- drawings and menus go from DCS.
  local invoke_s, invoke_other = ext_invoke('s'), ext_invoke('other')
  invoke_s.campaign, invoke_other.campaign = 8, 8
  play(ctx, { { op = 'CAMPAIGN', campaign = 8 }, invoke_s, invoke_other }, nil, 'a campaign switch')
  assert(rawget(server, 'stops') == 3, 'on_stop ran ' .. tostring(rawget(server, 'stops')) .. ' times in all, not 3')
  assert(next(shim.extensions) == nil, 'an extension survived the switch')
  assert(count(sim.marks) == 0 and #sim:menu_list('all') == 0, 'a mark or a menu survived the switch')
  assert(next(shim.marks) == nil and next(shim.menus) == nil, 'the shim holds a mark or a menu')
  assert(ctx.result(1).code == LUA_ERROR and ctx.result(2).code == LUA_ERROR, 'an extension answered after the switch')

  -- A simulation stop: on_stop from the hook's onSimulationStop, once; the
  -- next mission runs the chunk again with its counters at 0.
  play(ctx, { { op = 'EXTENSION_SET', name = 'again', rev = 1, source = stopper } }, nil, 'a new extension')
  local stops = rawget(server, 'stops')
  host:load_mission('second', { paused = true })
  assert(rawget(server, 'stops') == stops + 1, 'on_stop ran ' .. (rawget(server, 'stops') - stops) .. ' times at the stop')
  local e = shim.extensions.again
  assert(e and not e.dead and not e.stopped and e.calls == 0 and type(e.hooks.invoke) == 'function',
    'the extension did not start again with the next mission')
  host:load_mission('third', { paused = true })
  assert(rawget(server, 'stops') == stops + 2, 'on_stop did not run at the next stop')
  S.ok('on_stop runs for a replacement, a removal, a campaign switch (which also clears marks and menus) and a simulation stop, and the next mission runs the chunk again')
end, '\n[lua]\nraw = true\n')

scenario('menu-record', SHIM, function(ctx)
  local sim = running_world(ctx)
  local host = ctx.host
  local group_id = sim.groups['np!hGx1'].id
  local selected = false
  play(ctx, {
    menu(30, 1, 0, 1, 0, false, 'All'),
    menu(31, 1, 0, 2, 2, false, 'Coalition'),
    menu(32, 1, 0, 3, 1, false, 'Group'),
    ext_invoke('nobody', 600),
  }, function()
    if ctx.result(3) and not selected then
      selected = true
      sim:menu_select('all', { 'All' })
      sim:menu_select('c2', { 'Coalition' })
      sim:menu_select('g' .. group_id, { 'Group' })
    end
  end, 'menu selections')
  local got = printed(ctx, 'menu')
  assert(#got == 3, #got .. ' MENU records:\n' .. dump(ctx))
  local want = { { 30, 0, 1 }, { 31, 2, 2 }, { 32, 1, 3 } }
  for i, w in ipairs(want) do
    assert(got[i].entity == w[1] and got[i].target == w[2] and got[i].scope == w[3] and got[i].player == 0,
      'MENU ' .. i .. ' is ' .. json(got[i]))
  end
  S.ok('a selected menu item writes a MENU record with its entity, its target by scope, and player 0')
end, '\n[lua]\nraw = true\n')

for _, profile in ipairs(created) do
  S.remove(profile)
end
assert(ran, 'no scenario named ' .. ONLY)
