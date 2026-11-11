-- One ground unit: its identities, what its getters return, and what each
-- costs.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local origin = S.origin()
S.ground('np-one', 'M-113', { { name = 'np-one-1', x = origin.x - 200, z = origin.z - 200 } })

local polls = 0
return E.wait(function()
  -- Give the unit a frame to exist.
  polls = polls + 1
  if polls < 3 then
    return nil
  end
  local unit = Unit.getByName('np-one-1')
  if not unit then
    error('the unit did not spawn')
  end
  local id = unit:getObjectID()

  -- Unit.id_ is the runtime object ID; getID() is the mission ID, which
  -- starts at 1,000,000 for a runtime spawn (LIVE).
  E.observe('id_is_object_id', unit.id_ == id, { windows = true })
  local mission_id = unit:getID()
  -- A string under Wine for a unit added at run time (LIVE).
  E.observe('mission_id_type', type(mission_id), { wine = 'string' })
  E.observe('object_id_type', type(id), { info = true })
  E.observe('mission_id_is_runtime', (tonumber(mission_id) or 0) >= 1000000, { windows = true })
  E.observe('object_id_differs', tonumber(mission_id) ~= tonumber(id), { windows = true })

  -- DCS.getUnitPositionByObjectId returns x, y, z as three numbers, and
  -- agrees with getPoint (LIVE).
  local p = E.pack(DCS.getUnitPositionByObjectId(id))
  local point = unit:getPoint()
  E.observe('position_values', p.n, { windows = 3 })
  E.observe('position_matches_getPoint', p.n == 3 and math.abs(p[1] - point.x) < 0.01
    and math.abs(p[2] - point.y) < 0.01 and math.abs(p[3] - point.z) < 0.01, { windows = true })

  -- An M-113: life 3, fuel 1, and two ammunition entries of 104 and 404
  -- rounds (LIVE).
  E.observe('life', unit:getLife(), { windows = 3, within = 0.2 })
  E.observe('desc_life', unit:getDesc().life, { windows = 3 })
  E.observe('fuel', unit:getFuel(), { windows = 1 })
  local ammo, counts = unit:getAmmo(), {}
  for i, entry in ipairs(ammo or {}) do
    counts[i] = entry.count
  end
  E.observe('ammo_counts', counts, { windows = { 104, 404 } })
  E.observe('ammo_first_type', ammo and ammo[1] and ammo[1].desc and ammo[1].desc.typeName,
    { windows = 'weapons.shells.M2_12_7_T' })

  -- Costs on a rendering client, one unit, paused (LIVE there).
  local function cost(name, fn, baseline, n)
    E.observe(name .. '_ns', E.bench(fn, n or 20000, 3).raw_ns, { windows = baseline, timing = true })
  end
  cost('timer.getTime', timer.getTime, 40, 100000)
  cost('getLife', function() return unit:getLife() end, 85)
  cost('getFuel', function() return unit:getFuel() end, 85)
  cost('getName', function() return unit:getName() end, 85)
  cost('isExist', function() return unit:isExist() end, 135)
  cost('inAir', function() return unit:inAir() end, 135)
  cost('getPoint', function() return unit:getPoint() end, 800)
  cost('getVelocity', function() return unit:getVelocity() end, 760)
  cost('getPosition', function() return unit:getPosition() end, 3300)
  cost('getUnitPositionByObjectId', function() return DCS.getUnitPositionByObjectId(id) end, 60, 100000)
  cost('LoGetObjectById', function() return Export.LoGetObjectById(id) end, 9700, 5000)
  cost('LoGetWorldObjects', function() return Export.LoGetWorldObjects() end, 7000, 2000)

  S.destroy('np-one')
  return 'done'
end, 30, 'running')
