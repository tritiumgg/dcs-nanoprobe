-- E16: does the damage setter work on aircraft, ships and statics? Rule:
-- read back.
--
-- a_unit_set_life_percentage lives only in the `mission` state (ED-CODE:
-- MissionEditor/modules/me_trigrules.lua, as in lua/probes/53-damage.lua), so
-- this snippet runs in the hook state and crosses into `server` to add the
-- objects and read them, and into `mission` to set the life. Six subjects
-- stand apart from each other: an airborne F-15C, an F-15C parked cold at the
-- first airdrome, a UH-1H in the air, a speedboat on water (skipped with a
-- note when the map has none within 60 km), a static (a Hangar A) and a
-- ground unit as the control (an M-113, LIVE in lua/probes/53-damage.lua).
--
-- For each the setter is called with 50 and then with 0, and after each the
-- snippet reads isExist, getLife, getLife0, isDead and whether the unit is
-- still found by name, at 1 s, and after 0 at 1, 3 and 10 s. The unit-level
-- death events (S_EVENT_DEAD, UNIT_LOST, KILL, CRASH, EJECTION) that name a
-- subject are logged with their model time. The ID given to the setter for
-- a static is StaticObject:getID(); whether the setter takes that is part of
-- the result.
-- states: hook
-- timeout: 180

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    return 'CROSS FAILED ' .. tostring(value)
  end
  return value
end

local PRE = "local X = E.lib('s15')\nlocal S = X.S\nlocal o = S.origin()\n"

-- Spawn every subject. Returns text: the names of the subjects added, and
-- why any was skipped, as `name=kind` pairs and `skip:name:reason` entries.
local SPAWN = PRE .. [[
local out = {}
local function note(text) out[#out + 1] = text end
local cases = {}
E.store.e16 = { cases = cases, events = {}, t0 = timer.getTime() }
local store = E.store.e16
local function air(name, opts)
  opts.name = name
  local group, err = X.add_air(opts)
  if group then
    cases[#cases + 1] = { name = name .. '-1', label = name, kind = 'unit' }
    note(name .. '=unit')
  else
    note('skip:' .. name .. ':' .. tostring(err))
  end
end
air('np-e16-air', { x = o.x, z = o.z + 20000, alt = 6000 })
air('np-e16-heli', { kind = 'UH-1H', category = Group.Category.HELICOPTER, x = o.x - 5000, z = o.z + 20000, alt = 400,
  speed = 40, fuel = 300, task = 'Transport' })
-- A parked fighter: an open spot or a shelter that is free.
local base = S.airdrome()
local spot
for _, p in ipairs(base and base.base:getParking() or {}) do
  if (p.Term_Type == 104 or p.Term_Type == 72) and not p.TO_AC then spot = p break end
end
if spot then
  local cold = S.airdrome_point('cold', base)
  cold.x, cold.y = spot.vTerminalPos.x, spot.vTerminalPos.z
  air('np-e16-park', { x = spot.vTerminalPos.x, z = spot.vTerminalPos.z, alt = base.y, speed = 0, task = 'Nothing',
    parking = spot.Term_Index, points = { cold } })
else
  note('skip:np-e16-park:no free parking spot')
end
local water = S.water()
if water then
  local group, err = X.add_ground('np-e16-ship', 'speedboat', { { name = 'np-e16-ship-1', x = water.x, z = water.z } },
    { category = Group.Category.SHIP })
  if group then
    cases[#cases + 1] = { name = 'np-e16-ship-1', label = 'np-e16-ship', kind = 'unit' }
    note('np-e16-ship=unit')
  else
    note('skip:np-e16-ship:' .. tostring(err))
  end
else
  note('skip:np-e16-ship:no water within 60 km')
end
local group, err = X.add_ground('np-e16-ground', 'M-113', { { name = 'np-e16-ground-1', x = o.x + 3000, z = o.z + 12000 } })
if group then
  cases[#cases + 1] = { name = 'np-e16-ground-1', label = 'np-e16-ground', kind = 'unit' }
  note('np-e16-ground=unit')
end
local static, serr = X.try(function()
  return coalition.addStaticObject(country.id.USA, {
    name = 'np-e16-static', type = 'Hangar A', category = 'Fortifications', x = o.x + 3000, y = o.z + 12400, heading = 0,
  })
end)
if static then
  S.statics['np-e16-static'] = true
  cases[#cases + 1] = { name = 'np-e16-static', label = 'np-e16-static', kind = 'static' }
  note('np-e16-static=static')
else
  note('skip:np-e16-static:' .. tostring(serr))
end
local names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then names[id] = name end
end
E.track.handler({ onEvent = function(_, event)
  local who
  pcall(function() who = event.initiator:getName() end)
  if not who then pcall(function() who = event.target:getName() end) end
  if who and who:find('np-e16', 1, true) then
    store.events[#store.events + 1] = { t = timer.getTime() - store.t0, event = names[event.id] or event.id, unit = who }
  end
end })
return table.concat(out, ' ')
]]

-- The ID each subject takes for the setter, as `label=id` pairs.
local IDS = PRE .. [[
local out = {}
for _, c in ipairs(E.store.e16.cases) do
  local id
  if c.kind == 'unit' then
    local unit = Unit.getByName(c.name)
    id = unit and unit:getID()
  else
    local static = StaticObject.getByName(c.name)
    id = static and static:getID()
  end
  out[#out + 1] = c.label .. '=' .. tostring(id)
end
return table.concat(out, ' ')
]]

-- What each subject's reads return now.
local READ = PRE .. [[
local out = {}
for _, c in ipairs(E.store.e16.cases) do
  local r = {}
  local function put(key, fn) local v, err = X.try(fn) r[key] = v == nil and ('x: ' .. tostring(err)) or v end
  if c.kind == 'unit' then
    local unit = Unit.getByName(c.name)
    r.found_by_name = unit ~= nil
    if unit then
      put('isExist', function() return unit:isExist() end)
      put('isDead', function() return unit:isDead() end)
      put('life', function() return unit:getLife() end)
      put('life0', function() return unit:getLife0() end)
    end
  else
    local static = StaticObject.getByName(c.name)
    r.found_by_name = static ~= nil
    if static then
      put('isExist', function() return static:isExist() end)
      put('life', function() return static:getLife() end)
    end
  end
  out[c.label] = r
end
return X.enc(out)
]]

local EVENTS = PRE .. [[
return X.enc(E.store.e16.events)
]]

local started = DCS.getModelTime()
local ids, stage, mark = {}, 0, started

local function set_all(percent)
  local report = {}
  for label, id in pairs(ids) do
    local numeric = tonumber(id)
    if numeric then
      report[#report + 1] = label .. ':' .. cross('mission',
        'local ok, err = pcall(a_unit_set_life_percentage, ' .. numeric .. ', ' .. percent
          .. ') return ok and "ok" or tostring(err)')
    else
      report[#report + 1] = label .. ': no ID (' .. tostring(id) .. ')'
    end
  end
  table.sort(report)
  return report
end

local function record(name, source)
  E.observe(name, cross('server', source), { info = true })
end

E.observe('spawned', cross('server', SPAWN), { info = true })

-- (model seconds since the previous stage, action)
local STAGES = {
  { 3, function()
    local text = cross('server', IDS)
    for label, id in text:gmatch('(%S+)=(%S+)') do
      ids[label] = id
    end
    E.observe('ids', text, { info = true })
    record('before', READ)
  end },
  { 0.2, function() E.observe('set_50', set_all(50), { info = true }) end },
  { 1, function() record('after_50_plus_1_s', READ) end },
  { 2, function() E.observe('set_0', set_all(0), { info = true }) end },
  { 1, function() record('after_0_plus_1_s', READ) end },
  { 2, function() record('after_0_plus_3_s', READ) end },
  { 7, function() record('after_0_plus_10_s', READ) end },
  { 1, function() E.observe('events', cross('server', EVENTS), { info = true }) end },
  { 0, function()
    E.observe('cleared', cross('server', "local E = ...\nreturn tostring(E.lib('scenario').clear())\n"), { info = true })
  end },
}

return E.wait(function()
  local now = DCS.getModelTime()
  local stage_info = STAGES[stage + 1]
  if not stage_info then
    return 'done'
  end
  if now - mark < stage_info[1] then
    return nil
  end
  stage_info[2]()
  stage = stage + 1
  mark = now
  return nil
end, 120, 'running')
