-- timer.scheduleFunction in the server state: its quantum.
-- states: server
-- timeout: 60

local E = ...
local ticks, first, last, lag = 0, nil, nil, 0
local stop = false
local start = timer.getTime()

E.track.timer(function(_, t)
  if stop then
    return nil
  end
  ticks = ticks + 1
  first = first or t
  last = t
  lag = math.max(lag, timer.getTime() - t)
  return t + 0.0001
end, nil, start + 0.001)

return E.wait(function()
  if timer.getTime() - start < 2 then
    return nil
  end
  stop = true
  if not first or last == first then
    error('the timer ran ' .. ticks .. ' time(s) in 2 s of model time')
  end
  -- A timer that asks for 0.1 ms ran once per millisecond of model time:
  -- 6,119 ticks in 6.118 s (LIVE).
  E.observe('ticks_per_model_ms', (ticks - 1) / ((last - first) * 1000), { windows = 1, within = 0.02 })
  E.observe('ticks', ticks, { info = true })
  -- How far the time a callback is given runs behind timer.getTime().
  E.observe('largest_lag_s', lag, { info = true })
  return 'done'
end, 40, 'running')
