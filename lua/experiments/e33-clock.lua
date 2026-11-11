-- E33: the cost of one QueryPerformanceCounter read inside DCS. The rule:
-- recorded; it sets how many rows pass between clock reads (plan 6); more
-- than 1 us per read is raised with the owner.
-- states: hook server

local E = ...
local agent = E.lib('agent').open()
local best, frequency
for _ = 1, 5 do
  local ns, f = agent.qpc_cost(1000000)
  frequency = f
  if not best or ns < best then
    best = ns
  end
end
E.observe('qpc_ns', best, { timing = true })
E.observe('qpc_frequency', frequency, { info = true })
E.observe('under_1us', best < 1000, { windows = true })
-- The same read from Lua, through the entry: what the sampling loop pays if
-- it reads the clock itself.
local clock = agent.clock
local t0 = clock()
for _ = 1, 100000 do
  clock()
end
E.observe('clock_entry_ns', (clock() - t0) / 100000 * 1e9, { timing = true })
return 'done'
