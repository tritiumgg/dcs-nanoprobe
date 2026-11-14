-- E38: what the hook state's own reads say of a dead unit and a removed one.
--
-- The hook state has no scripting objects. It has DCS.getUnitProperty and
-- DCS.getUnitType, which take a mission ID, and DCS.getUnitPositionByObjectId
-- and Export.LoGetObjectById, which take an object ID. This snippet crosses
-- into `server` to add three carriers, kill the first with an explosion and
-- remove the second with Unit.destroy, then reads all three from the hook
-- state, and times each read on the unit that is alive.
-- states: hook
-- timeout: 60

local E = ...

local function cross(source)
  local ok, value = E.cross('server', source)
  return ok and value or ('CROSS FAILED ' .. tostring(value))
end

local function text(fn, ...)
  local result = { pcall(fn, ...) }
  if not result[1] then
    return 'raises'
  end
  local value = result[2]
  return type(value) == 'table' and 'table' or tostring(value)
end

cross([[
local S = E.lib('scenario')
local o = S.origin()
local units = {}
for i = 1, 3 do units[i] = { name = 'np-e38-h' .. i, x = o.x + 3000, z = o.z + 14000 + i * 60, heading = 0 } end
S.ground('np-e38-h', 'M-113', units)
return 'spawned'
]])

local IDS = [[
local out = {}
for i = 1, 3 do
  local unit = Unit.getByName('np-e38-h' .. i)
  if not unit then return '' end
  out[#out + 1] = tostring(unit:getID()) .. ':' .. tostring(rawget(unit, 'id_'))
end
return table.concat(out, ' ')
]]

local ACT = [[
trigger.action.explosion(Unit.getByName('np-e38-h1'):getPoint(), 300)
Unit.getByName('np-e38-h2'):destroy()
return 'acted'
]]

local function read(mission_id, object_id)
  local out = {
    position = text(DCS.getUnitPositionByObjectId, object_id),
    export_object = text(Export.LoGetObjectById, object_id),
    unit_type = text(DCS.getUnitType, mission_id),
  }
  for number = 1, 19 do
    out['property_' .. number] = text(DCS.getUnitProperty, mission_id, number)
  end
  return out
end

local units, step, wait_until = nil, 1, 0
return E.wait(function()
  local now = DCS.getRealTime()
  if now < wait_until then
    return nil
  end
  if step == 1 then
    local found = cross(IDS)
    if found == '' or found:find('CROSS FAILED', 1, true) then
      return nil
    end
    units = {}
    for mission_id, object_id in found:gmatch('(%d+):(%d+)') do
      units[#units + 1] = { mission_id = mission_id, object_id = tonumber(object_id) }
    end
    E.observe('alive_before', read(units[3].mission_id, units[3].object_id), { info = true })
    E.observe('act', cross(ACT), { info = true })
    step, wait_until = 2, now + 2
    return nil
  end
  E.observe('killed', read(units[1].mission_id, units[1].object_id), { info = true })
  E.observe('removed', read(units[2].mission_id, units[2].object_id), { info = true })
  E.observe('alive', read(units[3].mission_id, units[3].object_id), { info = true })
  local alive = units[3]
  E.observe('ns_a_call_warm', {
    getUnitProperty = E.bench(function() DCS.getUnitProperty(alive.mission_id, 1) end, 2000, 5).ns,
    getUnitType = E.bench(function() DCS.getUnitType(alive.mission_id) end, 2000, 5).ns,
    getUnitPositionByObjectId = E.bench(function() DCS.getUnitPositionByObjectId(alive.object_id) end, 2000, 5).ns,
  }, { timing = true })
  cross("E.lib('scenario').clear() return 'cleared'")
  return 'done'
end, 50, 'running')
