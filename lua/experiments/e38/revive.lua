-- E38: can a script bring back a unit that has ceased to exist and burns?
--
-- Four carriers 60 m apart. Three get an explosion beside them, so each
-- ceases to exist and burns; the fourth gets a strong explosion on it, which
-- sends its death events at once. Three seconds later the mission state's
-- a_unit_set_life_percentage sets the first to 100, the second to 50 and the
-- fourth to 100; the third is left alone. The snippet reads all four from
-- the `server` state at set times up to two minutes, and lists the world
-- events in between.
-- states: hook
-- timeout: 180

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  return ok and value or ('CROSS FAILED ' .. tostring(value))
end

cross('server', [[
local S = E.lib('scenario')
local o = S.origin()
for i = 1, 4 do
  S.ground('np-e38-v' .. i, 'M-113', { { name = 'np-e38-v' .. i .. '-1', x = o.x + 3000, z = o.z + 16000 + i * 60, heading = 0 } })
end
local names = {}
for name, id in pairs(world.event) do
  if type(id) == 'number' then names[id] = name end
end
E.store.e38_revive = { events = {}, handles = {} }
local store = E.store.e38_revive
E.track.handler({ onEvent = function(_, event)
  store.events[#store.events + 1] = names[event.id] or tostring(event.id)
end })
return 'spawned'
]])

local IDS = [[
local store, out = E.store.e38_revive, {}
for i = 1, 4 do
  local unit = Unit.getByName('np-e38-v' .. i .. '-1')
  if not unit then return '' end
  store.handles[i] = { id_ = rawget(unit, 'id_') }
  out[i] = tostring(unit:getID())
end
return table.concat(out, ' ')
]]

local EXPLODE = [[
for i = 1, 4 do
  local p = Unit.getByName('np-e38-v' .. i .. '-1'):getPoint()
  if i < 4 then
    trigger.action.explosion({ x = p.x, y = p.y, z = p.z - 6 }, 30)
  else
    trigger.action.explosion(p, 300)
  end
end
return 'exploded'
]]

-- Each unit's reads as text, then the events since the last call.
local READ = [[
local store, out = E.store.e38_revive, {}
local function mark(fn, handle)
  local ok, value = pcall(fn, handle)
  if not ok then return 'x' end
  if type(value) == 'boolean' then return value and '1' or '0' end
  return type(value) == 'number' and string.format('%.3g', value) or type(value)
end
for i = 1, 4 do
  local h = store.handles[i]
  out[i] = string.format('%d: exist %s dead %s life %s byName %s', i, mark(Object.isExist, h), mark(Unit.isDead, h),
    mark(Unit.getLife, h), tostring(Unit.getByName('np-e38-v' .. i .. '-1') ~= nil))
end
out[#out + 1] = 'events: ' .. table.concat(store.events, ' ')
for i = #store.events, 1, -1 do store.events[i] = nil end
return table.concat(out, '; ')
]]

local ids
local step, wait_until, exploded_at = 1, 0, nil
local READS_AT = { 2, 5, 10, 30, 60, 120 }
local next_read = 1

return E.wait(function()
  local now = DCS.getRealTime()
  if now < wait_until then
    return nil
  end
  if step == 1 then
    local text = cross('server', IDS)
    if text == '' or text:find('CROSS FAILED', 1, true) then
      return nil
    end
    ids = {}
    for id in text:gmatch('%S+') do
      ids[#ids + 1] = id
    end
    E.observe('before', cross('server', READ), { info = true })
    E.observe('explode', cross('server', EXPLODE), { info = true })
    exploded_at = now
    step, wait_until = 2, now + 3
    return nil
  end
  if step == 2 then
    E.observe('after_3_s', cross('server', READ), { info = true })
    E.observe('set_100_on_1', cross('mission', string.format("a_unit_set_life_percentage(%s, 100) return 'set'", ids[1])), { info = true })
    E.observe('set_50_on_2', cross('mission', string.format("a_unit_set_life_percentage(%s, 50) return 'set'", ids[2])), { info = true })
    E.observe('set_100_on_4', cross('mission', string.format("a_unit_set_life_percentage(%s, 100) return 'set'", ids[4])), { info = true })
    step = 3
    return nil
  end
  local due = READS_AT[next_read]
  if due and now - exploded_at >= 3 + due then
    E.observe('after_set_plus_' .. due .. '_s', cross('server', READ), { info = true })
    next_read = next_read + 1
  end
  if next_read > #READS_AT then
    cross('server', "E.lib('scenario').clear() return 'cleared'")
    return 'done'
  end
  return nil
end, 170, 'running')
