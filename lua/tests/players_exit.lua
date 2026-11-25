-- The rehearsal of the S16 live test on the host: the hook and the shim in the
-- strict mock, the host-native agent, and `command-probe --part players` as the
-- sidecar, over a real mapped file and a real loopback handshake. This script
-- plays what the live run's tool does: it reads the mark panels and selects the
-- menu command in the mock's server state, with the same snippets
-- (lua/experiments/s16/exit-*.lua), and it plays the DCS server's and the
-- scripted client's parts for the moderation scenario. The agent's state lasts
-- as long as the process, so tools/luatest.sh runs one scenario per process:
--
-- * functional-on:  raw Lua on. Menus, marks, drawings, messages, chat, an
--   extension (hooks, emit, invoke) across a MISSION_LOAD, EVAL.
-- * functional-off: raw Lua off. The same, and EXTENSION_SET and EVAL answer
--   RAW_LUA_OFF.
-- * mismatch:       raw Lua on, and the probe is told off: it fails the link
--   step.
-- * moderation:     a client joins, takes a slot, is moved to spectators, is
--   moved again in a race (E35), kicked, banned (the rejoin is refused), listed,
--   unbanned and rejoins. The mock repeats the kick and the slot move; the ban
--   that disconnects, and the refused rejoin, are this script's stand-in for
--   what E34 settles on a real server.
-- * moderation-fails: the same, and a rejoin after the ban is let through: the
--   probe fails `ban_refused`, unbans the client and stops.
-- * moves:          slot moves (spec/commands.md, "Slot moves"): a client joins
--   with no slot, the mission lists blue slot 8580, and the probe moves it, is
--   refused twice, and sends KEEP while this script changes the client to red.
--
-- The mark panel fields and what a ban does to a connected player are
-- UNVERIFIED; the live run settles them.

local S = dofile((arg[0]:match('^(.*/)') or './') .. 'support.lua')
local root = S.root()
local mock = dofile(root .. 'lua/mock/dcs.lua')

local MODULE, _, PROBE, _, _, SCENARIO = ...
local SCENARIOS = { 'functional-on', 'functional-off', 'mismatch', 'moderation', 'moderation-fails', 'moves', 'moves-occupied' }
local known = false
for _, name in ipairs(SCENARIOS) do
  known = known or name == SCENARIO
end
assert(MODULE and PROBE and known,
  'usage: lua lua/tests/players_exit.lua <module> <version> <link-probe> <inspect> <xtask> '
    .. table.concat(SCENARIOS, '|'))
local COMMAND_PROBE = PROBE:gsub('link%-probe$', 'command-probe')

local now = os.clock
local BUILD = '0123456789abcdef'
local TOKEN = string.rep('0123456789abcdef', 4)
local UCID = 'a1b2c3d4e5f60718293a4b5c6d7e8f90'

local profile = S.scratch()
local version = profile .. 'nanoprobe/versions/' .. BUILD .. '/'
local map = profile .. 'nanoprobe/link.map'
local control = profile .. 'control/'
local pidfile = profile .. 'probe.pid'

local function snippet(name)
  return S.read(root .. 'lua/experiments/s16/' .. name)
end

local function lines(path)
  local found = {}
  for _, line in ipairs(S.lines(path)) do
    local ok, e = pcall(S.json, line)
    if ok and type(e) == 'table' then
      found[#found + 1] = e
    end
  end
  return found
end

local function install(raw)
  S.mkdir(profile .. 'Scripts/Hooks')
  S.mkdir(version .. 'lua')
  S.mkdir(profile .. 'nanoprobe/logs')
  S.mkdir(control)
  S.copy(root .. 'lua/hook/nanoprobe-hook.lua', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  S.copy(root .. 'lua/agent/hook.lua', version .. 'lua/hook.lua')
  S.copy(root .. 'lua/agent/log.lua', version .. 'lua/log.lua')
  S.copy(root .. 'lua/agent/shim.lua', version .. 'lua/shim.lua')
  S.copy(MODULE, version .. 'nanoprobe_agent_' .. BUILD .. '.dll')
  S.write(profile .. 'nanoprobe/manifest.txt', 'active=' .. BUILD .. '\n')
  local port = 20000 + math.floor(os.clock() * 1e6 + os.time()) % 20000
  S.write(profile .. 'nanoprobe/agent.toml', string.format(
    "[transport]\nmap_path = '%s'\nport = %d\nout_ring_kib = 1024\nin_ring_kib = 256\n%s",
    map, port, raw and '\n[lua]\nraw = true\n' or ''))
  S.write(profile .. 'nanoprobe/token', TOKEN)
  return port
end

local function answer(file, text)
  S.write(control .. file, text or 'ok')
end

-- Frames, paced like the real simulation, until `done()` or `seconds` of this
-- process's time; whether it was done.
local function spin(host, seconds, done)
  local by = now() + seconds
  local next_frame = now()
  while now() < by do
    if now() >= next_frame then
      next_frame = next_frame + 0.01
      host:frame(0.01)
      if done and done() then
        return true
      end
    end
  end
  return done and done() or false
end

-- Run the probe against a fresh mock; `on_phase(host, e, out)` is called once
-- for each phase line; `each(host)` after every frame. Returns the verdict
-- line and every line.
local function run(args, raw, on_phase, each)
  local port = install(raw)
  local out = profile .. 'probe.jsonl'
  local host = mock.new({ writedir = profile })
  host:run('hook', profile .. 'Scripts/Hooks/nanoprobe-hook.lua')
  host:load_mission('first', { paused = false })
  assert(os.execute(string.format(
    '%s --part players --campaign 16 --map %s --port %d --token-file %s --control-dir %s '
      .. '--seconds 200 %s > %s 2>&1 & echo $! > %s',
    S.quote(COMMAND_PROBE), S.quote(map), port, S.quote(profile .. 'nanoprobe/token'),
    S.quote(control), args, S.quote(out), S.quote(pidfile))) == 0, 'could not start command-probe')

  local seen = {}
  local verdict
  local deadline = now() + 220
  local next_frame = now()
  while now() < deadline do
    if now() >= next_frame then
      next_frame = next_frame + 0.01
      host:frame(0.01)
      if each then
        each(host)
      end
      for _, e in ipairs(lines(out)) do
        if e.ev == 'phase' and not seen[e.name] then
          seen[e.name] = true
          on_phase(host, e, out)
        elseif e.ev == 'verdict' or e.ev == 'error' then
          verdict = e
        end
      end
      if verdict then
        break
      end
    end
  end
  local text = S.read(out) or ''
  -- S16_DUMP=1 prints what the probe wrote, for reading the rehearsal.
  if os.getenv('S16_DUMP') then
    io.write(text)
  end
  assert(verdict, 'no verdict within 220 s:\n' .. text:sub(-6000))
  return verdict, lines(out), text, host
end

local function steps_of(verdict)
  local by = {}
  for _, s in ipairs(verdict.steps) do
    by[s.name] = s
  end
  return by
end

-- What the live tool does for the three phases that need DCS.
local function tool_phases(host, e, out)
  local server = host.states.server
  if e.name == 'menus_set' then
    local picked
    for index = 0, 40 do
      server.missionCommands.doAction(index)
      spin(host, 0.3, function()
        for _, l in ipairs(lines(out)) do
          if l.ev == 'menu' and l.entity == e.command then
            return true
          end
        end
      end)
      for _, l in ipairs(lines(out)) do
        if l.ev == 'menu' and l.entity == e.command then
          picked = index
        end
      end
      if picked then
        break
      end
    end
    answer('menus_picked', picked and ('index=' .. picked) or 'unreachable: no index selected the item')
  elseif e.name == 'marks_set' or e.name == 'marks_removed' then
    local read = host.states.hook.net.dostring_in('server', snippet('exit-marks-read.lua'))
    assert(type(read) == 'string' and read:find('|panels=', 1, true), 'the mark read is ' .. tostring(read))
    answer(e.name == 'marks_set' and 'marks_read' or 'marks_gone', read)
  end
end

local function functional_run(raw, mismatch)
  -- The mission path as the live tool reads it from the hook state.
  local probe_host = mock.new({ writedir = profile })
  probe_host:load_mission('first', { paused = false })
  local mission_path = probe_host.states.hook.DCS.getMissionFilename()
  assert(type(mission_path) == 'string' and mission_path ~= '', 'the mock gave no mission path')
  local flag = mismatch and 'off' or (raw and 'on' or 'off')
  return run('--scenario functional --raw-lua ' .. flag .. ' --mission-path ' .. S.quote(mission_path),
    raw, tool_phases)
end

local function check_functional(raw)
  local verdict, all, text = functional_run(raw, false)
  assert(verdict.ev == 'verdict' and verdict.pass, 'failed:\n' .. text:sub(-8000))
  local by = steps_of(verdict)
  local function passed(name)
    assert(by[name] and by[name].verdict == 'pass', name .. ': ' .. tostring(by[name] and by[name].detail)
      .. '\n' .. text:sub(-5000))
  end
  for _, name in ipairs({ 'link', 'menu_set', 'menu_select', 'menu_remove', 'mark_set', 'marks_present',
    'mark_remove', 'marks_gone', 'message_all', 'message_coalition', 'chat_all', 'eval_gate' }) do
    passed(name)
  end
  assert(by.drawing_present and (by.drawing_present.verdict == 'pass' or by.drawing_present.verdict == 'note'))
  assert(verdict.raw_lua_seen == raw, 'the probe saw the wrong mode')
  if raw then
    for _, name in ipairs({ 'extension_set', 'extension_invoke', 'extension_emit', 'extension_record',
      'mission_load', 'running_after_load', 'extension_after_load', 'extension_frames_after_load',
      'extension_record_after_load', 'extension_emit_after_load', 'extension_remove' }) do
      passed(name)
    end
  else
    passed('extension_gate')
    assert(not by.mission_load, 'the raw-off run loaded the mission')
  end
  S.ok(string.format('functional, raw Lua %s: %d steps pass, %d noted, %d skipped',
    raw and 'on' or 'off', verdict.pass_steps, verdict.note_steps, verdict.skip_steps))
end

local function check_mismatch()
  local verdict, _, text = functional_run(true, true)
  assert(verdict.ev == 'verdict' and not verdict.pass, 'a run that expects the wrong mode passed')
  local by = steps_of(verdict)
  assert(by.link and by.link.verdict == 'fail' and by.link.detail:find('raw Lua on', 1, true), text:sub(-3000))
  S.ok('a probe told raw Lua is off, on an agent with it on, fails the link step')
end

-- The DCS server's and the client's parts of the moderation scenario.
local function moderation(let_rejoin_through)
  local ids = { 2, 3, 4, 5 }
  local connection = 0
  local handled = 0
  local function join(host)
    connection = connection + 1
    host:connect(ids[connection], { ucid = UCID, name = 'np-client', side = 0, slot = '' })
    return ids[connection]
  end
  local current
  -- A ban disconnects the player (UNVERIFIED on a real server).
  local function each(host)
    for i = handled + 1, #host.moderation do
      local m = host.moderation[i]
      if m.call == 'banlist_add' and host.players[m.args[1]] then
        host:disconnect(m.args[1], 0)
      end
    end
    handled = #host.moderation
  end
  local function on_phase(host, e, out)
    local n = e.name
    if n == 'ready' then
      current = join(host)
      answer('joined')
    elseif n == 'connected' then
      host:change_slot(current, 2, '8580')
      answer('slot_taken')
      answer('spawned')
    elseif n == 'slot_move_armed' then
      answer('slot_move_ready')
    elseif n == 'slot_moved' then
      answer('slot_checked')
    elseif n == 'e35_armed' then
      host:change_slot(current, 2, '8581')
      answer('e35_checked')
    elseif n == 'kick_armed' then
      answer('kick_ready')
    elseif n == 'kicked' then
      current = join(host)
      answer('rejoined')
    elseif n == 'ban_armed' then
      answer('ban_ready')
    elseif n == 'banned' then
      if let_rejoin_through then
        current = join(host)
        answer('rejoin_tried', 'joined')
      else
        answer('rejoin_tried', 'refused')
      end
    elseif n == 'unbanned' then
      current = join(host)
      answer('rejoined_after_unban')
    elseif n == 'finishing' then
      host:disconnect(current, 0)
      answer('left')
    end
  end
  return run('--scenario moderation', false, on_phase, each)
end

local function check_moderation()
  local verdict, all, text, host = moderation(false)
  assert(verdict.ev == 'verdict' and verdict.pass, 'failed:\n' .. text:sub(-8000))
  local by = steps_of(verdict)
  for _, name in ipairs({ 'link', 'players_query', 'connect', 'players_query_client', 'slot_record',
    'slot_to_spectators', 'e35_slot_race', 'kick', 'reconnect_after_kick', 'ban', 'ban_refused',
    'bans_lists_client', 'unban', 'bans_forgot_client', 'rejoin_after_unban' }) do
    assert(by[name] and by[name].verdict == 'pass', name .. ': ' .. tostring(by[name] and by[name].detail)
      .. '\n' .. text:sub(-5000))
  end
  -- E35's parts are numbers in milliseconds from the record. The command is
  -- queued before it is written; the next SLOT record may come before or after
  -- the RESULT, since the hook's handler changes the slot before it answers.
  local parts = S.json(by.e35_slot_race.detail)
  assert(parts.queued_ms and parts.written_ms and parts.result_ms and parts.slot_ms,
    'E35 gave no times: ' .. by.e35_slot_race.detail)
  assert(parts.queued_ms >= 0 and parts.queued_ms <= parts.written_ms and parts.queued_ms <= parts.result_ms,
    'E35 times are out of order: ' .. by.e35_slot_race.detail)
  -- The ucid is in no line the probe printed.
  assert(not text:find(UCID, 1, true), 'the probe printed a ucid')
  local calls = {}
  for _, m in ipairs(host.moderation) do
    calls[m.call] = (calls[m.call] or 0) + 1
  end
  assert(calls.kick == 1 and calls.banlist_add == 1 and calls.banlist_remove == 1
    and (calls.force_player_slot or 0) == 2,
    string.format('the moderation calls were kick %s, ban %s, unban %s, slot %s', tostring(calls.kick),
      tostring(calls.banlist_add), tostring(calls.banlist_remove), tostring(calls.force_player_slot)))
  assert(#host.bans == 0, 'a ban is left after UNBAN')
  S.ok(string.format('moderation: %d steps pass; E35 queued %.1f ms, result %.1f ms, slot %.1f ms after the record',
    verdict.pass_steps, parts.queued_ms, parts.result_ms, parts.slot_ms))
end

local function check_moderation_fails()
  local verdict, _, text, host = moderation(true)
  assert(verdict.ev == 'verdict' and not verdict.pass, 'a rejoin after a ban passed')
  local by = steps_of(verdict)
  assert(by.ban_refused and by.ban_refused.verdict == 'fail', text:sub(-3000))
  assert(not by.unban, 'the run went on after the failure')
  -- The probe unbans the client it banned, whatever happened.
  assert(#host.bans == 0, 'the ban is still there after the probe stopped')
  S.ok('a rejoin after a ban fails the run; the probe unbans the client and stops')
end

-- The DCS server's and the client's parts of the moves scenario.
local function check_moves()
  local current, go, frames = 2, false, 0
  local function each(host)
    if go then
      frames = frames + 1
      if frames == 50 then
        host:change_slot(current, 1, '')
      end
    end
  end
  local function on_phase(host, e)
    if e.name == 'ready' then
      host.slots = { red = {}, blue = { { unitId = '8580' } } }
      host:connect(current, { ucid = UCID, name = 'np-client', side = 0, slot = '' })
      answer('joined', '8580')
    elseif e.name == 'race_armed' then
      go = true
      answer('race_go')
    elseif e.name == 'race_done' then
      answer('race_checked')
    end
  end
  local verdict, _, text, host = run('--scenario moves', false, on_phase, each)
  assert(verdict.ev == 'verdict' and verdict.pass, 'failed:\n' .. text:sub(-8000))
  local by = steps_of(verdict)
  for _, name in ipairs({ 'link', 'players_query', 'players_query_client', 'connect', 'blue_no_slot', 'blue_slot', 'keep_leaves_slot',
    'other_coalition_refused', 'missing_slot_refused', 'red_no_slot', 'keep_takes_slot_coalition', 'keep_race',
    'spectators' }) do
    assert(by[name] and by[name].verdict == 'pass', name .. ': ' .. tostring(by[name] and by[name].detail)
      .. '\n' .. text:sub(-5000))
  end
  assert(by.other_coalition_refused.detail:find('the slot belongs to the other coalition', 1, true),
    by.other_coalition_refused.detail)
  -- The refused moves never reached DCS; every KEEP after the change kept red.
  for _, m in ipairs(host.moderation) do
    assert(m.call ~= 'force_player_slot' or m.args[3] ~= '99999999', 'the missing slot reached DCS')
    assert(m.call ~= 'force_player_slot' or not (m.args[2] == 1 and m.args[3] == '8580'),
      'the other coalition slot reached DCS')
  end
  assert(host.players[current].side == 0 and host.players[current].slot == '', 'the client is not a spectator at the end')
  S.ok('moves: ' .. verdict.pass_steps .. ' steps pass; ' .. by.keep_race.detail)
end

-- A player already connected: the moves scenario fails before any move.
local function check_moves_occupied()
  local function each(host)
    if not host.players[7] then
      host.players[7] = { ucid = 'b1b2c3d4e5f60718293a4b5c6d7e8f90', name = 'np-visitor', side = 2, slot = '', ping = 0 }
    end
  end
  local verdict, _, text, host = run('--scenario moves', false, function() end, each)
  assert(verdict.ev == 'verdict' and not verdict.pass, 'a run with a visitor passed')
  local by = steps_of(verdict)
  assert(by.players_query and by.players_query.verdict == 'fail', text:sub(-3000))
  for _, m in ipairs(host.moderation) do
    assert(m.call ~= 'force_player_slot', 'a visitor was moved')
  end
  S.ok('moves: a connected visitor fails players_query before any move')
end

local checks = {
  ['functional-on'] = function() check_functional(true) end,
  ['functional-off'] = function() check_functional(false) end,
  mismatch = check_mismatch,
  moderation = check_moderation,
  ['moderation-fails'] = check_moderation_fails,
  moves = check_moves,
  ['moves-occupied'] = check_moves_occupied,
}
local ok, err = pcall(checks[SCENARIO])
local pid = (S.read(pidfile) or ''):match('%d+')
if pid then
  os.execute('kill ' .. pid .. ' 2>/dev/null')
end
S.remove(profile)
assert(ok, err)
