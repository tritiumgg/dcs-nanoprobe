-- The pause matrix (plan experiment E17), version 0: what still works while
-- the simulation is paused. Load it with E.lib('pause') in the hook state.
--
-- Development only. The matrix drives the `server` state the way pump
-- candidate A would: the hook's onSimulationFrame makes one cross-state call
-- per frame. A probe first puts the simulation into a pause condition, then
-- hands M.start's step function to E.wait. While the pause holds, the steps
-- spawn a ground group and an airborne aircraft, read getters, count frames
-- and cross-state calls, and check whether a timer ran. Then they remove what
-- they spawned and record whether the removal finished while paused. If it
-- did not, they resume until it finishes and pause again.
--
-- A probe never calls this outside the hook state: E.cross works only there.

local M = {}

-- Seconds of real time the matrix holds the pause before it records.
local HOLD = 2
-- Seconds of real time it waits for a removal to finish while paused.
local REMOVAL = 1
-- Seconds of real time it then waits for the removal with the simulation
-- running.
local REMOVAL_RUNNING = 10

-- Spawn and arm, in the `server` state. NAME is set by the caller.
local SETUP = [[
  local S = E.lib('scenario')
  local origin = S.origin()
  local P = { births = 0, timer = 0, polls = 0 }
  E.store['pause_' .. NAME] = P
  E.track.handler({ onEvent = function(_, event)
    if event.id == world.event.S_EVENT_BIRTH then
      P.births = P.births + 1
    end
  end })
  E.track.timer(function()
    P.timer = P.timer + 1
  end, nil, timer.getTime() + 0.01)
  P.model = timer.getTime()
  local _, ground = S.ground(NAME, 'M-113', {
    { name = NAME .. '-1', x = origin.x - 600, z = origin.z - 300 },
    { name = NAME .. '-2', x = origin.x - 620, z = origin.z - 300 },
  })
  local _, air = S.spawn_aircraft({ name = NAME .. '-air', z = origin.z - 3000 })
  P.ground_ms, P.air_ms = ground * 1000, air * 1000
  return 'set'
]]

-- One per frame: count the call. NAME is set by the caller.
local POLL = [[
  local P = E.store['pause_' .. NAME]
  P.polls = P.polls + 1
  return P.polls
]]

-- What the getters give, as `key=value` pairs joined by `;`.
local READ = [[
  local P = E.store['pause_' .. NAME]
  local out = {}
  local function put(key, value)
    out[#out + 1] = key .. '=' .. tostring(value)
  end
  local unit = Unit.getByName(NAME .. '-1')
  put('unit', unit ~= nil)
  if unit then
    local p = unit:getPoint()
    put('point', type(p) == 'table' and type(p.x) == 'number')
    local x, _, z = DCS.getUnitPositionByObjectId(unit:getObjectID())
    put('by_id', type(x) == 'number')
    put('by_id_matches', type(x) == 'number' and math.abs(x - p.x) < 0.01 and math.abs(z - p.z) < 0.01)
    put('life', type(unit:getLife()) == 'number')
    put('fuel', type(unit:getFuel()) == 'number')
    put('exist', unit:isExist() == true)
    local v = unit:getVelocity()
    put('velocity', type(v) == 'table' and type(v.x) == 'number')
  end
  local group = Group.getByName(NAME)
  put('ground_units', group and #group:getUnits() or 0)
  local air = Unit.getByName(NAME .. '-air-1')
  put('air', air ~= nil and air:isExist() == true)
  if air then
    local p = air:getPoint()
    put('air_x', p.x)
    put('air_z', p.z)
    put('air_in_air', air:inAir())
  end
  put('model', timer.getTime())
  put('births', P.births)
  put('timer', P.timer)
  put('ground_ms', P.ground_ms)
  put('air_ms', P.air_ms)
  return table.concat(out, ';')
]]

local REMOVE = [[
  local S = E.lib('scenario')
  S.destroy(NAME)
  S.destroy(NAME .. '-air')
  return 'removed'
]]

local GONE = [[
  return Group.getByName(NAME) == nil and Group.getByName(NAME .. '-air') == nil
]]

-- `source` with NAME bound to `name`.
local function bind(name, source)
  return string.format('local NAME = %q\n', name) .. source
end

-- Run `source` in the `server` state, or raise with its error.
local function cross(E, name, source)
  local ok, value = E.cross('server', bind(name, source))
  if not ok then
    error('server: ' .. tostring(value), 2)
  end
  return value
end

-- `key=value;...` as a table of booleans, numbers and strings.
local function fields(text)
  local out = {}
  for key, value in tostring(text):gmatch('([%w_]+)=([^;]*)') do
    if value == 'true' then
      out[key] = true
    elseif value == 'false' then
      out[key] = false
    else
      out[key] = tonumber(value) or value
    end
  end
  return out
end

local function median(list)
  local sorted = { unpack(list) }
  table.sort(sorted)
  return sorted[math.ceil(#sorted / 2)]
end

-- Start the matrix for the condition `label`. The simulation must be paused.
-- Returns a step function for E.wait: nil until the matrix is done, then
-- 'done'. Every step runs from the hook's onSimulationFrame.
function M.start(E, label)
  if not DCS.getPause() then
    error('the pause matrix needs a paused simulation', 2)
  end
  local name = 'np-pause-' .. label
  local real0 = DCS.getRealTime()
  local model0 = DCS.getModelTime()
  local paused0 = E.hook.counts().frames_paused
  cross(E, name, SETUP)
  local first = fields(cross(E, name, READ))
  local steps, crossed, costs = 0, 0, {}
  local held = true
  local removed_at

  local resumed_at, repausing

  return function()
    local now = DCS.getRealTime()
    if repausing then
      -- DCS.setPause takes effect on a later frame (LIVE).
      return DCS.getPause() and 'done' or nil
    end
    if resumed_at then
      local gone = cross(E, name, GONE) == 'true'
      if not gone and now - resumed_at < REMOVAL_RUNNING then
        return nil
      end
      E.observe('removal_done_after_resume', gone, { windows = true })
      E.observe('removal_seconds_after_resume', gone and (now - resumed_at) or nil, { info = true })
      DCS.setPause(true)
      repausing = true
      return nil
    end
    if removed_at then
      local gone = cross(E, name, GONE) == 'true'
      if not gone and now - removed_at < REMOVAL then
        return nil
      end
      -- Group:destroy() finishes later than the call (LIVE). Whether it
      -- finishes while paused is what this records. A removal still
      -- waiting is let finish with the simulation running, so nothing is
      -- left behind, and the pause is set again.
      E.observe('removal_done_while_paused', gone, { info = true })
      if gone then
        return 'done'
      end
      DCS.setPause(false)
      resumed_at = now
      return nil
    end
    held = held and DCS.getPause() == true

    steps = steps + 1
    local ok, polls = E.cross('server', bind(name, POLL))
    costs[#costs + 1] = (DCS.getRealTime() - now) * 1e6
    if ok and tonumber(polls) == crossed + 1 then
      crossed = crossed + 1
    end
    if now - real0 < HOLD then
      return nil
    end

    local last = fields(cross(E, name, READ))
    local frames = E.hook.counts().frames_paused - paused0
    E.observe('condition', label, { info = true })
    E.observe('held_paused', held, { windows = true })
    -- A per-frame hook callback keeps running while paused (LIVE, seen
    -- indirectly on a rendering client).
    E.observe('frames_while_paused', frames > 0, { windows = true })
    E.observe('paused_frames', frames, { info = true })
    E.observe('cross_every_frame', steps > 0 and crossed == steps, { windows = true })
    E.observe('cross_calls', crossed, { info = true })
    E.observe('cross_us_median', median(costs), { info = true })
    E.observe('cross_us_max', math.max(unpack(costs)), { info = true })
    -- Model time stands still and timers do not run while paused (LIVE).
    E.observe('model_time_stood_still', DCS.getModelTime() == model0 and last.model == first.model,
      { windows = true })
    E.observe('timer_ran', last.timer, { windows = 0 })
    -- One airborne F-15C spawned through a cross-state call at model time 0
    -- (LIVE, one case). The rest of this row is new.
    E.observe('spawn_ground', last.ground_units == 2, { windows = true })
    E.observe('spawn_air', last.air == true, { windows = true })
    E.observe('spawn_ground_ms', last.ground_ms, { info = true })
    E.observe('spawn_air_ms', last.air_ms, { info = true })
    E.observe('births_while_paused', last.births, { info = true })
    local getters = first.point and first.by_id and first.life and first.fuel and first.exist
      and first.velocity and last.point and last.by_id and true or false
    E.observe('getters', getters, { windows = true })
    E.observe('by_id_matches_point', last.by_id_matches == true, { windows = true })
    E.observe('air_stood_still', first.air_x == last.air_x and first.air_z == last.air_z, { windows = true })
    E.observe('air_in_air', last.air_in_air, { info = true })
    -- Pump candidate A needs all of these in every pause condition.
    local a = held and frames > 0 and crossed == steps and getters and last.ground_units == 2
      and last.air == true
    E.observe('candidate_a', a and true or false, { windows = true })

    cross(E, name, REMOVE)
    removed_at = now
    return nil
  end
end

return M
