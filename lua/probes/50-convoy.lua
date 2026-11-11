-- A moving ground group, captured, destroyed and added again where it was:
-- the convoy must carry on along its route.
-- states: server
-- timeout: 120

local E = ...
local S = E.lib('scenario')
local name, route = S.spawn_convoy()

local function lead()
  return Unit.getByName(name .. '-1')
end
local function distance(a, b)
  return math.sqrt((a.x - b.x) ^ 2 + (a.z - b.z) ^ 2)
end

local step, mark = 1, timer.getTime()
local start, restored, ids

return E.wait(function()
  local unit = lead()
  if step == 1 then
    -- Give the group a moment to exist before taking its start point.
    if timer.getTime() - mark < 1 or not unit then
      return nil
    end
    start = unit:getPoint()
    step, mark = 2, timer.getTime()
  elseif step == 2 then
    if timer.getTime() - mark < 10 then
      return nil
    end
    local point = unit:getPoint()
    -- The group moved at 7 to 8 m/s and its controller had a task (LIVE).
    E.observe('moved_m', distance(point, start), { info = true })
    E.observe('moving', distance(point, start) > 20, { windows = true })
    E.observe('has_task', unit:getGroup():getController():hasTask(), { windows = true })

    -- Capture each unit, destroy the group, and add it again in the same
    -- call with the route cut to "here, then the points still ahead".
    local units = {}
    for i, u in ipairs(unit:getGroup():getUnits()) do
      local p, x = u:getPoint(), u:getPosition().x
      units[i] = { name = u:getName(), x = p.x, z = p.z, heading = math.atan2(x.z, x.x) }
    end
    ids = { object = unit:getObjectID(), mission = tostring(unit:getID()) }
    local ahead = { { x = units[1].x, z = units[1].z, speed = route[1].speed } }
    for _, p in ipairs(route) do
      if p.x > units[1].x + 1 then
        ahead[#ahead + 1] = p
      end
    end
    local group = unit:getGroup()
    group:destroy()
    -- The destroyed group still exists within the call (LIVE).
    E.observe('exists_right_after_destroy', group:isExist(), { windows = true })
    S.ground(name, 'M-113', units, ahead)
    restored = point
    step, mark = 3, timer.getTime()
  elseif step == 3 then
    if timer.getTime() - mark < 10 or not unit then
      return nil
    end
    -- The runtime object ID changed and the mission ID stayed (LIVE).
    E.observe('object_id_changed', unit:getObjectID() ~= ids.object, { windows = true })
    E.observe('mission_id_kept', tostring(unit:getID()) == ids.mission, { windows = true })
    -- It carried on along the route: 45 m further after 9.8 s (LIVE).
    local point = unit:getPoint()
    E.observe('moved_after_restore_m', distance(point, restored), { info = true })
    E.observe('continued', distance(point, restored) > 20 and point.x > restored.x, { windows = true })
    S.destroy(name)
    return 'done'
  end
  return nil
end, 100, 'running')
