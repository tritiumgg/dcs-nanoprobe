-- The cost of the cross-state call, in parts, for S11's decision on plan
-- candidate C (calling the shim from the DLL, with no net.dostring_in):
-- `empty` is net.dostring_in('server', 'return'); `noop` is the hook's pump
-- string calling a pump that does nothing. Each is the median and p99 of 400
-- calls in one frame, in µs, timed with the agent's clock. The full path,
-- the hook's real pump call, is the agent's own estimate `xcall_us` in its
-- `sampling` reports.
-- states: hook
-- timeout: 60

local E = ...
local a = E.lib('agent').open()
local clock = a.clock
local N = 400

local function times(chunk)
  local t = {}
  for i = 1, N do
    local t0 = clock()
    net.dostring_in('server', chunk)
    t[i] = (clock() - t0) * 1e6
  end
  table.sort(t)
  return { median = t[math.floor(N / 2)], p99 = t[math.floor(N * 0.99)] }
end

net.dostring_in('server', '__nanoprobe_noop = { pump = function() end }')
local empty = times('return')
local noop = times('local s = __nanoprobe_noop if s and s.pump then s.pump() end')
net.dostring_in('server', '__nanoprobe_noop = nil')
return { empty = empty, noop = noop, calls = N }
