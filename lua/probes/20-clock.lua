-- The resolution of the two clocks a state can read.
-- states: hook server

local E = ...

-- The smallest step a clock makes, over 20 tries.
local function resolution(read)
  local best
  for _ = 1, 20 do
    local a = read()
    local b, n = a, 0
    while b == a and n < 5000000 do
      b = read()
      n = n + 1
    end
    if b ~= a and (not best or b - a < best) then
      best = b - a
    end
  end
  return best
end

-- os.clock has 1 ms resolution; DCS.getRealTime about 100 ns (LIVE).
E.observe('os_clock_resolution_s', resolution(os.clock), { windows = 0.001, timing = true })
E.observe('real_time_resolution_s', resolution(DCS.getRealTime), { windows = 1e-7, timing = true })
