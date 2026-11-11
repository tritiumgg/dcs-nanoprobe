-- What a cross-state call costs from the hook.
-- states: hook

local E = ...

local function ns(fn, n)
  return E.bench(fn, n, 3).raw_ns
end

-- Best-of minima on a rendering client at 60 fps (LIVE there).
E.observe('dostring_return_1_ns', ns(function()
  return net.dostring_in('server', 'return 1')
end, 20000), { windows = 2000, timing = true })
E.observe('dostring_timer_ns', ns(function()
  return net.dostring_in('server', 'return tostring(timer.getTime())')
end, 20000), { windows = 3000, timing = true })
E.observe('getModelTime_ns', ns(DCS.getModelTime, 200000), { windows = 30, timing = true })
E.observe('getRealTime_ns', ns(DCS.getRealTime, 200000), { info = true })
