-- The cost of each DCS call a birth's reads make, and of the position
-- getters, per unit, in µs: the best of 7 passes over up to 300 existing
-- units, each pass one net.dostring_in from the hook state timed with the
-- agent's clock, less an empty loop's pass. Also what Unit:getCategory
-- returns in this build. Reads only; leaves nothing behind.
-- states: hook
-- timeout: 120
local E = ...
local a = E.lib('agent').open()
local clock = a.clock
local n = net.dostring_in('server', [[
__np_diag = {}
for side = 0, 2 do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    for _, u in ipairs(g:getUnits() or {}) do
      if #__np_diag < 300 and u:isExist() then __np_diag[#__np_diag + 1] = u end
    end
  end
end
return tostring(#__np_diag)
]])
n = tonumber(n) or 0
local function best(chunk)
  local b = math.huge
  for _ = 1, 7 do
    local t0 = clock()
    net.dostring_in('server', chunk)
    local t = (clock() - t0) * 1e6
    if t < b then b = t end
  end
  return b
end
local empty = best('for _, u in ipairs(__np_diag) do end')
local calls = {
  isExist = 'u:isExist()', getName = 'u:getName()', getTypeName = 'u:getTypeName()',
  getGroup = 'u:getGroup()', group_getID = 'u:getGroup():getID()',
  group_getName = 'u:getGroup():getName()', group_getCategory = 'u:getGroup():getCategory()',
  getCoalition = 'u:getCoalition()', getCountry = 'u:getCountry()', getDesc = 'u:getDesc()',
  getDesc_category = 'u:getDesc().category', getCategory = 'u:getCategory()',
  getLife0 = 'u:getLife0()', getLife = 'u:getLife()', getFuel = 'u:getFuel()',
  getPoint = 'u:getPoint()', getPosition = 'u:getPosition()',
  byObjectId = 'DCS.getUnitPositionByObjectId(u.id_)', id_ = 'rawget(u, "id_")',
}
local out = { units = n, empty_loop_us = empty }
for k, c in pairs(calls) do
  local t = best('for _, u in ipairs(__np_diag) do local x = ' .. c .. ' end')
  out[k] = n > 0 and (t - empty) / n or 0
end
-- What getCategory returns in this build: one value or two.
out.getCategory_values = net.dostring_in('server', 'local u = __np_diag[1] if not u then return "" end local a, b = u:getCategory() local g = u:getGroup():getCategory() return tostring(a) .. "," .. tostring(b) .. " group " .. tostring(g) .. " desc " .. tostring(u:getDesc().category)')
net.dostring_in('server', '__np_diag = nil')
return out
