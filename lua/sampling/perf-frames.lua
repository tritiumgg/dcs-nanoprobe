-- What a small batch costs when it runs once a frame, as sampling does,
-- with the rest of the simulation between two batches: per row, in µs, the
-- median and p99 over 240 frames. In the hook state: the position getter
-- driven from the DLL (`csweep` mode 2) over 10, 20, 40 and 80 unit IDs, and
-- in a Lua loop over 20. In the server state, through one cross-state call:
-- `getLife` and `getFuel` driven from the DLL over 20 and 80 units, with the
-- call's own time apart. Each frame takes the next rows of the table, so a
-- pass covers every unit. Reads only; leaves nothing behind.
-- states: hook
-- timeout: 300
local E = ...
local a = E.lib('agent').open()
local clock, csweep = a.clock, a.csweep
local FRAMES = 240

local function server(source)
  local text, ok = net.dostring_in('server', source)
  if ok == false then
    error('server: ' .. tostring(text), 2)
  end
  return text
end

-- The unit IDs, sent over in pieces; the server state keeps the units.
local count = tonumber(server([[
local units, ids = {}, {}
for side = 0, 2 do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    for _, u in ipairs(g:getUnits() or {}) do
      if u:isExist() then
        units[#units + 1] = u
        ids[#ids + 1] = u.id_
      end
    end
  end
end
-- The agent's table in this state is the shim's.
local ok, agent = pcall(function() return __nanoprobe_shim and __nanoprobe_shim.agent end)
__np_frames = { units = units, ids = ids, agent = ok and agent or nil, list = {} }
return tostring(#ids)
]])) or 0
local ids = {}
local from = 1
while from <= count do
  local text = server(string.format(
    'local c = __np_frames.ids return table.concat(c, ",", %d, math.min(#c, %d))', from, from + 399))
  for id in tostring(text):gmatch('[^,]+') do
    ids[#ids + 1] = tonumber(id)
  end
  from = from + 400
end
local in_server = server('return tostring(__np_frames.agent ~= nil)') == 'true'

local getter = DCS.getUnitPositionByObjectId
local n = #ids
local cursor = 0
local list = {}

-- The next `rows` IDs, in a table of exactly that length.
local function take(rows)
  for i = #list, rows + 1, -1 do
    list[i] = nil
  end
  for i = 1, rows do
    cursor = cursor % n + 1
    list[i] = ids[cursor]
  end
  return list
end

local function stats(t)
  table.sort(t)
  if #t == 0 then
    return { n = 0 }
  end
  return { n = #t, median = t[math.ceil(#t / 2)], p90 = t[math.ceil(#t * 0.9)], p99 = t[math.ceil(#t * 0.99)] }
end

-- The server half of a frame: `rows` units from the cursor, life and fuel
-- driven from the DLL, timed inside the chunk.
local VITALS = [[
local s = __np_frames
local ag, units, list = s.agent, s.units, s.list
local rows = %d
for i = #list, rows + 1, -1 do list[i] = nil end
local c = s.cursor or 0
for i = 1, rows do
  c = c %% #units + 1
  list[i] = units[c]
end
s.cursor = c
local t0 = ag.clock()
ag.csweep(units[1].getLife, list, 1, 1)
ag.csweep(units[1].getFuel, list, 1, 1)
return tostring((ag.clock() - t0) * 1e6)
]]

local cases = {
  { name = 'hook_csweep_10', rows = 10 },
  { name = 'hook_csweep_20', rows = 20 },
  { name = 'hook_csweep_40', rows = 40 },
  { name = 'hook_csweep_80', rows = 80 },
  { name = 'hook_lua_20', rows = 20, lua = true },
  { name = 'server_vitals_20', rows = 20, vitals = true },
  { name = 'server_vitals_80', rows = 80, vitals = true },
}
local times = {}
for _, c in ipairs(cases) do
  times[c.name] = {}
  if c.vitals then
    times[c.name .. '_call'] = {}
  end
end

local frame = 0
-- What the server half last answered in place of a time.
local failed
return E.wait(function()
  if n == 0 or type(getter) ~= 'function' then
    server('__np_frames = nil')
    return { units = n, has_getter = type(getter) }
  end
  frame = frame + 1
  for _, c in ipairs(cases) do
    if c.vitals then
      if in_server then
        local chunk = string.format(VITALS, c.rows)
        local t0 = clock()
        local text = net.dostring_in('server', chunk)
        local total = (clock() - t0) * 1e6
        local inner = tonumber(text)
        if inner then
          local t = times[c.name]
          t[#t + 1] = inner / c.rows
          t = times[c.name .. '_call']
          t[#t + 1] = total - inner
        else
          failed = tostring(text)
        end
      end
    elseif c.lua then
      local l = take(c.rows)
      local t0 = clock()
      for i = 1, c.rows do
        local _, _, _ = getter(l[i])
      end
      times[c.name][frame] = (clock() - t0) * 1e6 / c.rows
    else
      local l = take(c.rows)
      local t0 = clock()
      csweep(getter, l, 3, 2)
      times[c.name][frame] = (clock() - t0) * 1e6 / c.rows
    end
  end
  if frame < FRAMES then
    return nil
  end
  local out = { units = n, frames = frame, server_agent = in_server, error = failed }
  for name, t in pairs(times) do
    out[name] = stats(t)
  end
  server('__np_frames = nil')
  a.sink_take()
  return out
end, 240, 'running')
