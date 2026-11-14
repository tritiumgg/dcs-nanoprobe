-- E38: what a unit object, DCS.getUnitProperty and the export calls offer.
--
-- One tank and one carrier. The snippet records the names the Unit class
-- holds, what the reads of life return on the object and on a plain table
-- `{ id_ = <object ID> }`, which property numbers DCS.getUnitProperty
-- accepts and by which ID, the field names of an Export.LoGetObjectById
-- object, and the shape of getAmmo's rows.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local o = S.origin()
S.ground('np-e38-q', 'M-1 Abrams', { { name = 'np-e38-q1', x = o.x + 3000, z = o.z + 18000, heading = 0 } })
S.ground('np-e38-p', 'M-113', { { name = 'np-e38-p1', x = o.x + 3000, z = o.z + 18100, heading = 0 } })

local function text(fn, ...)
  local result = { pcall(fn, ...) }
  if not result[1] then
    return 'raises'
  end
  local value = result[2]
  return type(value) == 'table' and 'table' or tostring(value)
end

local function names(t)
  local list = {}
  for name, value in pairs(t) do
    list[#list + 1] = tostring(name) .. ':' .. type(value)
  end
  table.sort(list)
  return list
end

return E.wait(function()
  local tank, carrier = Unit.getByName('np-e38-q1'), Unit.getByName('np-e38-p1')
  if not tank or not carrier then
    return nil
  end
  local object_id, mission_id = rawget(tank, 'id_'), tonumber(tank:getID())
  local plain = { id_ = object_id }
  E.observe('unit_object_fields', names(tank), { info = true })
  E.observe('unit_class', names(getmetatable(tank)), { info = true })
  local calls = {}
  for _, name in ipairs({ 'getLife', 'getLife0', 'isBroken', 'isDead', 'isEffective', 'isAlive', 'isActive', 'getFuel' }) do
    calls[name] = { object = text(function() return Unit[name](tank) end), plain_table = text(function() return Unit[name](plain) end) }
  end
  calls.isExist = { object = text(Object.isExist, tank), plain_table = text(Object.isExist, plain) }
  calls.desc_life = { object = text(function() return tank:getDesc().life end) }
  E.observe('reads_of_life', calls, { info = true })

  -- Which property numbers answer, by mission ID and by object ID. The
  -- values are the mission's own names, so only their kind is kept.
  local function properties(id)
    local valid, invalid, empty = {}, {}, {}
    for number = 0, 40 do
      local value = text(DCS.getUnitProperty, id, number)
      local list = (value == '!invalid property!' and invalid) or (value == '' and empty) or valid
      list[#list + 1] = number
    end
    return { answered = valid, empty = empty, invalid = invalid }
  end
  E.observe('getUnitProperty_by_mission_id', properties(mission_id), { info = true })
  E.observe('getUnitProperty_by_object_id', properties(object_id), { info = true })

  local exported
  local ok, object = pcall(Export.LoGetObjectById, object_id)
  if ok and type(object) == 'table' then
    exported = names(object)
    if type(object.Flags) == 'table' then
      E.observe('export_object_flags', names(object.Flags), { info = true })
    end
  end
  E.observe('export_object_fields', exported or text(Export.LoGetObjectById, object_id), { info = true })

  local function ammo(unit)
    local rows = Unit.getAmmo(unit)
    local out = { rows = type(rows) == 'table' and #rows or text(Unit.getAmmo, unit) }
    if type(rows) == 'table' and rows[1] then
      out.row_fields = names(rows[1])
      out.desc_fields = type(rows[1].desc) == 'table' and names(rows[1].desc) or nil
    end
    return out
  end
  E.observe('getAmmo_tank', ammo(tank), { info = true })
  E.observe('getAmmo_carrier', ammo(carrier), { info = true })
  S.clear()
  return 'done'
end, 40, 'running')
