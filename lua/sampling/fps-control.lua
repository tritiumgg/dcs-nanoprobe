-- DCS's frame rate over MEASURE seconds of real time without the agent, for
-- the soak's control run (`cargo xtask soak control`). The eval hook polls a
-- waiting snippet once a frame (lua/dev/eval-hook.lua, `frame`), so each
-- call of the wait is one frame. Returns the fields of lua/sampling/fps.lua:
-- the mean and the 5th percentile of one-second windows' frame rates, and
-- model time over real time.
-- states: hook
-- timeout: 300

local E = ...
local MEASURE = tonumber('@MEASURE@') or 10
local r0, m0, last_r
local n, last_n = 0, 0
local windows = {}
return E.wait(function()
  local now = DCS.getRealTime()
  if DCS.getPause() then
    return nil
  end
  if not r0 then
    r0, m0, last_r = now, DCS.getModelTime(), now
    return nil
  end
  n = n + 1
  if now - last_r >= 1 then
    windows[#windows + 1] = (n - last_n) / (now - last_r)
    last_n, last_r = n, now
  end
  if now - r0 >= MEASURE then
    table.sort(windows)
    local p5 = windows[math.max(1, math.floor(#windows * 0.05 + 0.5))] or 0
    return {
      fps = n / (now - r0),
      p5_fps = p5,
      model_per_real = (DCS.getModelTime() - m0) / (now - r0),
      windows = #windows,
    }
  end
end, MEASURE + 120, 'running')
