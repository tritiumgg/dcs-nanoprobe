-- Waypoint Script actions. Rule: record what fires, when, with what, and at
-- what cost.
--
-- ED-CODE: a waypoint's task is a ComboTask whose `tasks` hold a
-- WrappedAction with `params.action = { id = 'Script', params = { command =
-- '<lua>' } }` (MissionEditor/modules/me_mission.lua:857 and :1094 read and
-- write that shape; the Script command is declared at
-- MissionEditor/modules/me_action_db.lua:2335-2352: "Run lua script. The
-- group will be passed as a single parameter to the function.").
--
-- Run 2: each command text is self-contained and wrapped so that nothing can
-- raise (run 1's command called a global that did not exist in the state that
-- runs Script commands, and DCS stopped answering after the error):
--   pcall(function(g) <body> end, ...)
-- The body creates `_G.__np_wp` on first use, appends one line (n, group name
-- through g, model time, cost, and whether `world`, `net`, `a_do_script`,
-- `trigger` exist in the state that ran it) and writes `env.info('np_wp ...')`
-- when env exists. The table is read back in the `server` state and in the
-- `mission` state (net.dostring_in from the hook) to say which one ran it.
-- Two M-113 groups (np-w4 with 4 units, np-w1 with 1) get a 4-point off-road
-- route; points 1 (the start), 2 and 3 carry the self-contained command (see
-- above) numbered by n, point 4 none; the log line holds n, the group name,
-- model time and its own cost. The snippet records: when each fires
-- (once per group or per unit, on arrival or on passing, at point 1), what
-- happens while the simulation is paused (paused from the hook state, held
-- and read), and what a re-route of np-w4 midway does (the new route's points
-- are numbered 11, 12, 13: do the old 2 and 3 still fire?). A world event
-- handler logs every field of S_EVENT_UNIT_CREATE_TASK (44),
-- S_EVENT_UNIT_TASK_COMPLETE (49) and S_EVENT_UNIT_TASK_STAGE (50) and is
-- removed at the end.
--
-- The hook's `E.wait(..., 'running')` resumes a paused simulation every
-- two seconds (lua/dev/eval-hook.lua:292-302); this snippet does not pass
-- 'running', so its own pause holds, and it resumes the simulation itself.
-- states: hook
-- timeout: 300

local E = ...

local function cross(source)
  local ok, value = E.cross('server', source)
  if not ok then
    return 'CROSS FAILED ' .. tostring(value)
  end
  return value
end

local PRE = [[
local S = E.lib('scenario')
local o = S.origin()
local CX, CZ = o.x + 9000, o.z + 9000
local function ser(v, depth)
  depth = depth or 0
  if type(v) ~= 'table' then
    return type(v) == 'string' and ('"' .. v .. '"') or tostring(v)
  end
  if depth > 2 then
    return '{...}'
  end
  local keys = {}
  for k in pairs(v) do
    keys[#keys + 1] = k
  end
  table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
  local parts = {}
  for _, k in ipairs(keys) do
    parts[#parts + 1] = tostring(k) .. '=' .. ser(v[k], depth + 1)
  end
  return '{' .. table.concat(parts, ',') .. '}'
end
local function clock()
  local ok, agent = pcall(function() return E.lib('agent').open() end)
  if ok and agent and agent.clock then
    return agent.clock()
  end
  return os.clock()
end
]]

-- The command text. N is the point number. Wrapped in pcall; no outside names.
local function command(n)
  return 'pcall(function(g) local c0 = os and os.clock and os.clock() or 0; '
    .. 'local T = _G.__np_wp or {}; _G.__np_wp = T; '
    .. 'local name = "?"; pcall(function() name = g:getName() end); '
    .. 'local size = "?"; pcall(function() size = g:getSize() end); '
    .. 'local mt = "?"; pcall(function() mt = string.format("%.2f", timer.getTime()) end); '
    .. 'local c1 = os and os.clock and os.clock() or 0; '
    .. 'local line = "n=' .. n .. ' group=" .. tostring(name) .. " arg=" .. type(g) .. " size=" .. tostring(size) '
    .. '.. " model=" .. mt .. " cost_us=" .. string.format("%.1f", (c1 - c0) * 1e6) '
    .. '.. " world=" .. type(world) .. " net=" .. type(net) .. " a_do_script=" .. type(a_do_script) .. " trigger=" .. type(trigger); '
    .. 'T[#T + 1] = line; '
    .. 'if env and env.info then env.info("np_wp " .. line) end '
    .. 'end, ...)'
end
local INIT = PRE .. [[
local W = { events = {} }
E.store.wp = W
-- (np_wp is not a global any more: the command text carries its own body.)

local EVENTS = { [44] = 'UNIT_CREATE_TASK', [49] = 'UNIT_TASK_COMPLETE', [50] = 'UNIT_TASK_STAGE' }
W.handler = {
  onEvent = function(_, event)
    local name = EVENTS[event.id]
    if not name then
      return
    end
    local row = { event = name, model = string.format('%.2f', timer.getTime()) }
    for k, v in pairs(event) do
      local tv = type(v)
      if tv == 'number' or tv == 'string' or tv == 'boolean' then
        row[tostring(k)] = v
      elseif tv == 'table' then
        local ok, text = pcall(ser, v)
        row[tostring(k)] = ok and text or 'table'
      else
        local ok, nm = pcall(function() return v:getName() end)
        row[tostring(k)] = tv .. (ok and (' ' .. tostring(nm)) or '')
      end
    end
    W.events[#W.events + 1] = ser(row)
  end,
}
world.addEventHandler(W.handler)
return 'handler added'
]]

local function commands_prefix()
  local parts = { 'local COMMANDS = {}' }
  for _, n in ipairs({ 1, 2, 3, 11, 12, 13 }) do
    parts[#parts + 1] = string.format('COMMANDS[%d] = %q', n, command(n))
  end
  return table.concat(parts, '\n') .. '\n'
end

local SPAWN = PRE .. commands_prefix() .. [[
local function script_task(n)
  return {
    id = 'ComboTask',
    params = { tasks = { {
      number = 1, auto = false, enabled = true, id = 'WrappedAction',
      params = { action = { id = 'Script', params = { command = COMMANDS[n] } } },
    } } },
  }
end
local function point(x, z, n)
  return {
    x = x, y = z, type = 'Turning Point', action = 'Off Road', speed = 10, speed_locked = true,
    ETA = 0, ETA_locked = false, formation_template = '',
    task = n and script_task(n) or { id = 'ComboTask', params = { tasks = {} } },
  }
end
local function add(name, count, dx)
  local units = {}
  for i = 1, count do
    units[i] = { name = name .. '-' .. i, type = 'M-113', x = CX + dx + (i - 1) * 12, y = CZ, heading = math.pi / 2,
      skill = 'Average', playerCanDrive = false }
  end
  local data = {
    name = name, task = 'Ground Nothing', visible = false, start_time = 0, units = units,
    x = units[1].x, y = units[1].y,
    route = { points = {
      point(CX + dx, CZ, 1), point(CX + dx, CZ + 300, 2), point(CX + dx + 300, CZ + 300, 3), point(CX + dx + 300, CZ, nil),
    } },
  }
  local ok, err = pcall(coalition.addGroup, country.id.USA, Group.Category.GROUND, data)
  S.groups[name] = true
  return name .. '=' .. tostring(ok) .. (ok and '' or (' ' .. tostring(err)))
end
return add('np-w4', 4, 0) .. ' ' .. add('np-w1', 1, 600)
]]

local READ = PRE .. [[
local W = E.store.wp
local T = rawget(_G, '__np_wp') or {}
local function pos(name)
  local u = Unit.getByName(name)
  if not u then return 'gone' end
  local p = u:getPoint()
  return string.format('(%.0f,%.0f)', p.x - CX, p.z - CZ)
end
return table.concat({
  'model ' .. string.format('%.1f', timer.getTime()),
  'calls(server state) ' .. #T, 'events ' .. #W.events,
  'w4-1 ' .. pos('np-w4-1') .. ' w4-4 ' .. pos('np-w4-4') .. ' w1-1 ' .. pos('np-w1-1'),
}, ' | ')
]]

local REROUTE = PRE .. commands_prefix() .. [[
local g = Group.getByName('np-w4')
local u = Unit.getByName('np-w4-1')
if not g or not u then return 'no group' end
local p = u:getPoint()
local function script_task(n)
  return { id = 'ComboTask', params = { tasks = { { number = 1, auto = false, enabled = true, id = 'WrappedAction',
    params = { action = { id = 'Script', params = { command = COMMANDS[n] } } } } } } }
end
local function pt(x, z, n)
  return { x = x, y = z, type = 'Turning Point', action = 'Off Road', speed = 10, speed_locked = true, ETA = 0,
    ETA_locked = false, formation_template = '', task = script_task(n) }
end
local ok, err = pcall(function()
  g:getController():setTask({ id = 'Mission', params = { route = { points = {
    pt(p.x, p.z, 11), pt(p.x, p.z - 300, 12), pt(p.x - 300, p.z - 300, 13),
  } } } })
end)
return 'reroute from (' .. string.format('%.0f,%.0f', p.x - CX, p.z - CZ) .. ') ' .. tostring(ok) .. ' ' .. tostring(err)
]]

local FINAL = PRE .. [[
local W = E.store.wp
pcall(world.removeEventHandler, W.handler)
local T = rawget(_G, '__np_wp') or {}
local out = { 'np_wp lines in the server state (' .. #T .. '):' }
for _, l in ipairs(T) do out[#out + 1] = '  ' .. l end
_G.__np_wp = nil
out[#out + 1] = 'events (' .. #W.events .. '):'
for _, l in ipairs(W.events) do out[#out + 1] = '  ' .. l end
S.clear()
return table.concat(out, '\n')
]]

local function record(name, source)
  E.observe(name, cross(source), { info = true })
end

local steps, step, mark = {}, 0, DCS.getRealTime()
-- after(real seconds, fn): fn returns true when done, or false to be called
-- again on the next frame (until give_up_s of real time have passed).
local function after(wait_s, fn, give_up_s)
  steps[#steps + 1] = { wait_s, fn, give_up_s or 0 }
end
after(0, function() E.observe('start_paused', DCS.getPause(), { info = true }); DCS.setPause(false); return true end)
after(0, function() return not DCS.getPause() end, 10)
after(0, function() record('init', INIT); return true end)
after(0, function() record('spawn', SPAWN); return true end)
after(5, function() record('read_5s', READ); return true end)
after(15, function() record('read_20s', READ); return true end)
after(2, function() record('read_22s_before_pause', READ); DCS.setPause(true); return true end)
after(0, function() return DCS.getPause() == true end, 10)
after(3, function()
  E.observe('pause_in_hook_3s_after', DCS.getPause(), { info = true })
  record('read_paused_a', READ)
  return true
end)
after(4, function()
  E.observe('pause_in_hook_7s_after', DCS.getPause(), { info = true })
  record('read_paused_b', READ)
  DCS.setPause(false)
  return true
end)
after(0, function() return not DCS.getPause() end, 10)
after(10, function() record('read_resumed_10s', READ); return true end)
after(10, function() record('reroute', REROUTE); return true end)
for i = 1, 8 do
  after(10, function() record('read_after_reroute_' .. (i * 10) .. 's', READ); return true end)
end
after(0, function()
  local ok, text = pcall(net.dostring_in, 'mission',
    "local T = rawget(_G, '__np_wp') or {} local r = 'lines in the mission state: ' .. #T .. '\\n' .. table.concat(T, '\\n') _G.__np_wp = nil return r")
  E.observe('mission_state_table', tostring(ok) .. ' ' .. tostring(text), { info = true })
  return true
end)
after(0, function() record('log_events_cleanup', FINAL); return true end)

return E.wait(function()
  local now = DCS.getRealTime()
  local s = steps[step + 1]
  if not s then
    return 'done'
  end
  if now - mark < s[1] then
    return nil
  end
  local done = s[2]()
  if not done then
    s[4] = s[4] or now
    if s[3] > 0 and now - s[4] > s[3] then
      E.observe('step_timeout', step + 1, { info = true })
    else
      return nil
    end
  end
  step = step + 1
  mark = DCS.getRealTime()
  return nil
end, 290)
