-- The parts of one position row of the shim's pump, per row, in µs: the
-- best of 7 passes over the shim's own table (`__nanoprobe_shim.T`, up to
-- 2,000 rows), each pass one net.dostring_in from the hook state timed with
-- the agent's clock, less an empty loop's pass. `C call` is the agent's
-- `wants`, a sink-sized call into the DLL that changes nothing. Reads only.
-- states: hook
-- timeout: 120
local E = ...
local a = E.lib('agent').open()
local clock = a.clock
local setup = [[
local s = __nanoprobe_shim
__np_rows = {}
if s and s.T and s.T.ids then
  for slot, id in pairs(s.T.ids) do
    if #__np_rows < 2000 and s.T.unit[slot] then __np_rows[#__np_rows + 1] = slot end
  end
end
return tostring(#__np_rows)
]]
local n = tonumber((net.dostring_in('server', setup))) or 0
local head = 'local s = __nanoprobe_shim local T, ag = s.T, s.agent local rows = __np_rows '
  .. 'local cur for i = 1, #rows do local slot = rows[i] '
local tail = ' end'
local function best(body)
  local b = math.huge
  for _ = 1, 7 do
    local t0 = clock()
    net.dostring_in('server', head .. body .. tail)
    local t = (clock() - t0) * 1e6
    if t < b then b = t end
  end
  return b
end
local empty = best('')
local parts = {
  slot_upvalue = 'cur = slot',
  table_reads = 'local u, id = T.unit[slot], T.ids[slot]',
  dcs_lookup = 'local f = rawget(_G, "DCS") and rawget(DCS, "getUnitPositionByObjectId")',
  getter = 'local x, y, z = DCS.getUnitPositionByObjectId(T.ids[slot])',
  c_call = 'ag.wants(slot)',
  c_call_4_args = 'ag.wants(slot, 1.5, 2.5, 3.5)',
  row_without_sink = 'cur = slot if T.unit[slot] then local f = rawget(_G, "DCS") and rawget(DCS, "getUnitPositionByObjectId") if f then local x, y, z = f(T.ids[slot]) end end',
  row_with_c_call = 'cur = slot local x, y, z if T.unit[slot] then local f = rawget(_G, "DCS") and rawget(DCS, "getUnitPositionByObjectId") if f then x, y, z = f(T.ids[slot]) end end ag.wants(slot, x, y, z)',
}
local out = { rows = n, empty_loop_us = empty }
for k, body in pairs(parts) do
  out[k] = n > 0 and (best(body) - empty) / n or 0
end
net.dostring_in('server', '__np_rows = nil')
return out
