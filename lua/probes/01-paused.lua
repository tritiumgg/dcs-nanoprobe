-- What still runs while the simulation is paused. Runs only when the suite
-- finds the simulation paused, as a server with no client is.
-- states: hook
-- timeout: 30

local E = ...
if not DCS.getPause() then
  E.note('the simulation is not paused; nothing observed')
  return 'skipped'
end

local frames = E.hook.counts().frames
local model = DCS.getModelTime()
local start = DCS.getRealTime()
local ok, why = E.cross('server', [[
  E.store.paused_timer = 0
  E.track.timer(function() E.store.paused_timer = 1 end, nil, timer.getTime() + 0.01)
  return 'set'
]])
if not ok then
  error('could not schedule the timer: ' .. tostring(why))
end

return E.wait(function()
  if DCS.getRealTime() - start < 1 then
    return nil
  end
  -- A per-frame hook callback keeps running while paused (LIVE, seen
  -- indirectly on a rendering client).
  E.observe('frames_while_paused', E.hook.counts().frames - frames > 0, { windows = true })
  -- Model time stands still and timers do not run while paused (LIVE).
  E.observe('model_time_stood_still', DCS.getModelTime() == model, { windows = true })
  local _, fired = E.cross('server', 'return E.store.paused_timer')
  E.observe('timer_fired_while_paused', fired, { windows = '0' })
  E.observe('still_paused', DCS.getPause(), { info = true })
  return 'done'
end, 10)
