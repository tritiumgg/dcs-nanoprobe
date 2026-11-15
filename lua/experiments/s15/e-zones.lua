-- Run-time trigger zones. Rule: record what each call accepts, returns and
-- does.
--
-- ED-CODE (Scripts/GeneratedTasks/modules/helicopter_descent_blue.lua:237-283
-- and :515-517): `trigger.misc.addZone(zone_data)` takes the Mission Editor's
-- zone table (x, y, radius, color {r,g,b,a}, properties {{key,value}},
-- hidden, name, linked_object_name) and returns a table whose `zoneId` the
-- caller uses in `c_part_of_group_in_zone(group, zoneId)` of a trigger it adds
-- with `trigger.misc.addTrigger`.
--
-- Run 2 changes the drive and the pause check: two zones of 150 m, np-za
-- alone and np-zb with an addTrigger on c_part_of_group_in_zone; an M-113 for
-- each starts 210 m (60 m outside) before the centre and drives straight
-- through to 210 m past it, long enough to enter and leave; every field of
-- each S_EVENT_TRIGGER_ZONE event is recorded (read with pcall); the pause
-- is set from the hook state, polled with DCS.getPause until true, then
-- getZone is read in the server state, then the simulation resumes.
-- (Run 1: add the zone in the Mission Editor's form, a minimal form and two
-- guessed positional forms; read each back with getZone (by name, and by
-- id); add a trigger on the zone and drive an M-113 in; record
-- S_EVENT_TRIGGER_ZONE (35) events (their fields) and the flag the trigger
-- sets; read the zone while paused and after resuming; list the functions
-- of trigger.misc and try any that removes or changes a zone; add the zone
-- again under its name with another radius. The snippet cannot remove a
-- zone that has no remove call: it says so. Whether a zone is gone after a
-- mission reload is read by the caller with a second, tiny eval.
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
local function ser(v, depth)
  depth = depth or 0
  if type(v) ~= 'table' then
    return type(v) == 'string' and ('"' .. v .. '"') or tostring(v)
  end
  if depth > 3 then
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
local CX, CZ = o.x + 8000, o.z + 8000
local function try(fn, ...)
  local r = { pcall(fn, ...) }
  if r[1] then
    local out = {}
    for i = 2, #r do
      out[#out + 1] = ser(r[i])
    end
    if #out == 0 then
      out[1] = 'nothing'
    end
    return 'ok ' .. table.concat(out, ' | ')
  end
  return 'ERR ' .. tostring(r[2])
end
]]


local INIT = PRE .. [[
local out = {}
E.store.zones = { events = {} }
E.track.handler({
  onEvent = function(_, event)
    if event.id ~= world.event.S_EVENT_TRIGGER_ZONE then
      return
    end
    local row = { t = timer.getTime() }
    for k, v in pairs(event) do
      local tv = type(v)
      if tv == 'number' or tv == 'string' or tv == 'boolean' then
        row[tostring(k)] = v
      elseif tv == 'table' then
        local ok, text = pcall(ser, v)
        row[tostring(k)] = ok and text or 'table'
      else
        local ok, name = pcall(function() return v:getName() end)
        row[tostring(k)] = tv .. (ok and (' ' .. tostring(name)) or '')
      end
    end
    E.store.zones.events[#E.store.zones.events + 1] = ser(row)
  end,
})
local za = trigger.misc.addZone({ name = 'np-za', x = CX, y = CZ, radius = 150, hidden = false, linked_object_name = 0,
  color = { 1, 0, 0, 0.15 }, properties = { { key = 'np', value = 'a' } } })
local zb = trigger.misc.addZone({ name = 'np-zb', x = CX + 1500, y = CZ, radius = 150, hidden = false, linked_object_name = 0,
  color = { 0, 1, 0, 0.15 }, properties = { { key = 'np', value = 'b' } } })
E.store.zones.za, E.store.zones.zb = za.zoneId, zb.zoneId
return 'za=' .. ser(za) .. ' zb=' .. ser(zb) .. ' | getZone za ' .. try(trigger.misc.getZone, 'np-za')
]]

local SPAWN = PRE .. [[
local Z = E.store.zones
-- each unit starts 210 m west of its zone centre: 60 m outside the 150 m zone
S.ground('np-ga', 'M-113', { { name = 'np-ua', x = CX, z = CZ - 210 } },
  { { x = CX, z = CZ - 210, speed = 0 }, { x = CX, z = CZ + 210, speed = 10 } })
S.ground('np-gb', 'M-113', { { name = 'np-ub', x = CX + 1500, z = CZ - 210 } },
  { { x = CX + 1500, z = CZ - 210, speed = 0 }, { x = CX + 1500, z = CZ + 210, speed = 10 } })
local gb = Group.getByName('np-gb')
Z.gb = gb and gb:getID()
local r = 'trigger on zb: ' .. try(trigger.misc.addTrigger, {
  actions = 'a_set_flag("np-zb-hit");',
  func = 'if mission.trig.conditions[*]() then if not mission.trig.flag[*] then mission.trig.actions[*](); mission.trig.flag[*] = true;end; else mission.trig.flag[*] = false; end;',
  flag = true,
  conditions = string.format('return(c_part_of_group_in_zone(%d, %d) )', Z.gb, Z.zb),
})
return r
]]

local READ = PRE .. [[
local function dist(name, cz)
  local u = Unit.getByName(name)
  if not u then return 'gone' end
  local p = u:getPoint()
  return string.format('%.0f', math.sqrt((p.x - CX - (cz or 0)) ^ 2 + (p.z - CZ) ^ 2))
end
return table.concat({
  'model ' .. string.format('%.1f', timer.getTime()),
  'ua dist ' .. dist('np-ua', 0) .. ', ub dist ' .. dist('np-ub', 1500),
  'flag np-zb-hit = ' .. try(trigger.misc.getUserFlag, 'np-zb-hit'),
  'events so far = ' .. #E.store.zones.events,
}, ' | ')
]]

local FINAL = PRE .. [[
local out = { 'events (' .. #E.store.zones.events .. '):' }
for _, e in ipairs(E.store.zones.events) do
  out[#out + 1] = '  ' .. e
end
S.clear()
return table.concat(out, '\n')
]]

local GETZONE = PRE .. [[ return try(trigger.misc.getZone, 'np-za') .. ' / ' .. try(trigger.misc.getZone, 'np-zb') ]]

local function record(name, source)
  E.observe(name, cross(source), { info = true })
end

local steps, step, mark = {}, 0, DCS.getRealTime()
-- after(real seconds, fn): fn returns true when done, or nil/false to be called
-- again on the next frame, up to the wait given.
local function after(wait_s, fn, give_up_s)
  steps[#steps + 1] = { wait_s, fn, give_up_s or 0 }
end
local waited = 0
after(0, function() E.observe('start_paused', DCS.getPause(), { info = true }); DCS.setPause(false); return true end)
after(0, function() return not DCS.getPause() end, 10)
after(0, function() record('init', INIT); return true end)
after(2, function() DCS.setPause(true); return true end)
after(0, function()
  local p = DCS.getPause()
  if p then
    E.observe('pause_confirmed_in_hook', p, { info = true })
    return true
  end
end, 10)
after(1, function()
  E.observe('pause_read_in_hook', DCS.getPause(), { info = true })
  record('getZone_while_paused', GETZONE)
  return true
end)
after(0, function() DCS.setPause(false); return true end)
after(0, function() return not DCS.getPause() end, 10)
after(0, function() record('spawn', SPAWN); return true end)
for i = 1, 7 do
  after(10, function() record('read_' .. (i * 10) .. 's', READ); return true end)
end
after(0, function() record('events_and_cleanup', FINAL); return true end)

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
    waited = waited + (now - (E.last_poll or now))
    E.last_poll = now
    s[4] = s[4] or now
    if s[3] > 0 and now - s[4] > s[3] then
      E.observe('step_timeout', step + 1, { info = true })
    else
      return nil
    end
  end
  step = step + 1
  mark = DCS.getRealTime()
  E.last_poll = nil
  return nil
end, 280, 'running')
