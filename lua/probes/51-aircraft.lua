-- An aircraft in flight, captured, destroyed and added again with half its
-- fuel and half its gun rounds.
-- states: server
-- timeout: 120

local E = ...
local S = E.lib('scenario')
local _, cost = S.spawn_aircraft({ fuel = 3000, gun = 50 })
-- One airborne F-15C took 6.6 ms to add (LIVE on a client).
E.observe('addGroup_ms', cost * 1000, { windows = 6.6, timing = true })

local function aircraft()
  return Unit.getByName('np-air-1')
end
local function gun(unit)
  for _, entry in ipairs(unit:getAmmo() or {}) do
    if entry.desc and entry.desc.typeName == 'weapons.shells.M61_20_HE' then
      return entry.count
    end
  end
  return nil
end

local step, mark = 1, timer.getTime()
local captured

return E.wait(function()
  local unit = aircraft()
  if step == 1 then
    if timer.getTime() - mark < 2 or not unit then
      return nil
    end
    -- 3,000 kg of 6,103 kg and half the gun rounds (LIVE).
    E.observe('fuel', unit:getFuel(), { windows = 0.4896, within = 0.01 })
    E.observe('fuel_mass_max', unit:getDesc().fuelMassMax, { windows = 6103 })
    E.observe('gun_rounds', gun(unit), { windows = 470 })
    E.observe('in_air', unit:inAir(), { windows = true })
    -- On the client the controller had no task for a plain route. Under
    -- Wine, with the group task CAP that this scenario gives, it has one.
    -- Which of the two differences decides it is not known (LIVE on both).
    E.observe('has_task', unit:getGroup():getController():hasTask(), { windows = false, wine = true })
    local point = unit:getPoint()
    local p = E.pack(DCS.getUnitPositionByObjectId(unit:getObjectID()))
    E.observe('position_matches_getPoint', p.n == 3 and math.abs(p[1] - point.x) < 1 and math.abs(p[3] - point.z) < 1,
      { windows = true })

    local position, velocity = unit:getPosition(), unit:getVelocity()
    captured = {
      x = point.x,
      z = point.z,
      alt = point.y,
      heading = math.atan2(position.x.z, position.x.x),
      speed = math.sqrt(velocity.x ^ 2 + velocity.y ^ 2 + velocity.z ^ 2),
      fuel = unit:getFuel() * unit:getDesc().fuelMassMax / 2,
      gun = 25,
    }
    unit:getGroup():destroy()
    S.spawn_aircraft(captured)
    step, mark = 2, timer.getTime()
  elseif step == 2 then
    if timer.getTime() - mark < 15 or not unit then
      return nil
    end
    -- After 15 s: 3,343 m along the route at 220 m/s, fuel 0.2432, and 235
    -- rounds (LIVE).
    local point = unit:getPoint()
    E.observe('flown_m', math.sqrt((point.x - captured.x) ^ 2 + (point.z - captured.z) ^ 2),
      { windows = 3343, within = 400 })
    E.observe('fuel_after_restore', unit:getFuel(), { windows = 0.2432, within = 0.01 })
    E.observe('gun_rounds_after_restore', gun(unit), { windows = 235 })
    E.observe('altitude_kept', math.abs(point.y - captured.alt) < 300, { info = true })
    S.destroy('np-air')
    return 'done'
  end
  return nil
end, 100, 'running')
