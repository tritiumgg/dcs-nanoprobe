-- DCS's own frame rate, for the S10 results: settle for SETTLE seconds of
-- real time with the simulation running, then count frames for MEASURE
-- seconds, one window a second. Returns the mean and the 5th percentile of
-- the windows' frame rates (the worst sustained), and model time over real
-- time. The frame count is the agent's, so it needs the agent loaded.
-- states: hook
-- timeout: 300

local E = ...
local SETTLE, MEASURE = tonumber('@SETTLE@') or 30, tonumber('@MEASURE@') or 60
local a = E.lib('agent').open()
local start, f0, r0, m0, last_f, last_r
local windows = {}
return E.wait(function()
  local now = DCS.getRealTime()
  if DCS.getPause() then
    return nil
  end
  if not start then
    start = now
    return nil
  end
  if not f0 then
    if now - start >= SETTLE then
      f0, r0, m0 = a.status().frame, now, DCS.getModelTime()
      last_f, last_r = f0, now
    end
    return nil
  end
  local f = a.status().frame
  if now - last_r >= 1 then
    windows[#windows + 1] = (f - last_f) / (now - last_r)
    last_f, last_r = f, now
  end
  if now - r0 >= MEASURE then
    table.sort(windows)
    local p5 = windows[math.max(1, math.floor(#windows * 0.05 + 0.5))] or 0
    return {
      fps = (f - f0) / (now - r0),
      p5_fps = p5,
      model_per_real = (DCS.getModelTime() - m0) / (now - r0),
      windows = #windows,
    }
  end
end, SETTLE + MEASURE + 120, 'running')
