-- E5: getter sweeps driven from C against the Lua loop that feeds a C sink.
-- 2,000 idle M-113s. Every engine call in the C-driven shape runs under
-- lua_pcall. The rule: adopt C-driven only if it is at least 2x faster.
-- states: server
-- timeout: 300

local E = ...
local S = E.lib('scenario')
local agent = E.lib('agent').open()
local read = agent.clock
local sink, csweep = agent.sink, agent.csweep

local N = 2000

-- Milliseconds for one pass of `fn`, best of seven.
local function ms(fn)
  local best
  for _ = 1, 7 do
    local t0 = read()
    fn()
    local t = read() - t0
    if not best or t < best then
      best = t
    end
  end
  return best * 1000
end

local groups = S.spawn_grid(N, { label = 'e5', offset = 1500 })
local units, ids = {}, {}
for _, name in ipairs(groups) do
  for _, unit in ipairs(Group.getByName(name):getUnits()) do
    units[#units + 1] = unit
    ids[#ids + 1] = unit.id_
  end
end
E.observe('units', #units, { windows = N })

local function compare(name, getter, list, nres)
  local plain = ms(function()
    for i = 1, #list do
      getter(list[i])
    end
  end)
  local lua_sink = ms(function()
    for i = 1, #list do
      sink(getter(list[i]))
    end
  end)
  local c_driven = ms(function()
    csweep(getter, list, nres)
  end)
  agent.sink_take()
  E.observe(name .. '.lua_loop_ms', plain, { timing = true })
  E.observe(name .. '.lua_loop_sink_ms', lua_sink, { timing = true })
  E.observe(name .. '.c_driven_ms', c_driven, { timing = true })
  E.observe(name .. '.speedup', lua_sink / c_driven, { info = true })
  return lua_sink / c_driven
end

local best = 0
best = math.max(best, compare('byObjectId', DCS.getUnitPositionByObjectId, ids, 3))
best = math.max(best, compare('getLife', units[1].getLife, units, 1))
best = math.max(best, compare('getFuel', units[1].getFuel, units, 1))
best = math.max(best, compare('getPoint', units[1].getPoint, units, 1))
E.observe('best_speedup', best, { info = true })
E.observe('adopt_c_driven', best >= 2, { info = true })

for _, name in ipairs(groups) do
  S.destroy(name)
end

return E.wait(function()
  for _, name in ipairs(groups) do
    if Group.getByName(name) then
      return nil
    end
  end
  return 'done'
end, 120, 'running')
