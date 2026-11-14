-- The position getters' cost by kind of unit, in µs per call: moving units
-- (speed over 0.5 m/s), halted units, and halted units of the most common
-- type alone, so the getter's cost on moving units can be told from its cost
-- on idle ones (E5 measured 0.17 µs on 2,000 idle M-113s). For each class:
-- `DCS.getUnitPositionByObjectId`, `getPoint`, and `getPosition`, which the
-- stopped tier calls once per row. The best of 7 passes over up to 500 units
-- a class, each pass one net.dostring_in from the hook state timed with the
-- agent's clock, less an empty loop's pass. Reads only; leaves nothing
-- behind.
-- states: hook
-- timeout: 180
local E = ...
local a = E.lib('agent').open()
local clock = a.clock
local setup = [[
local moving, halted, types = {}, {}, {}
for side = 0, 2 do
  for _, g in ipairs(coalition.getGroups(side) or {}) do
    for _, u in ipairs(g:getUnits() or {}) do
      if u:isExist() then
        local v = u:getVelocity()
        local speed = math.sqrt(v.x * v.x + v.y * v.y + v.z * v.z)
        if speed > 0.5 then
          if #moving < 500 then moving[#moving + 1] = u end
        else
          if #halted < 500 then halted[#halted + 1] = u end
          local t = u:getTypeName()
          types[t] = types[t] or {}
          if #types[t] < 500 then types[t][#types[t] + 1] = u end
        end
      end
    end
  end
end
local best, top = nil, 0
for t, list in pairs(types) do
  if #list > top then best, top = t, #list end
end
__np_classes = { moving = moving, halted = halted, idle_same_type = best and types[best] or {} }
return string.format('%d,%d,%d,%s', #moving, #halted, top, tostring(best))
]]
local counts = net.dostring_in('server', setup)
local function best(class, body)
  local b = math.huge
  for _ = 1, 7 do
    local t0 = clock()
    net.dostring_in('server', 'local l = __np_classes.' .. class
      .. ' for i = 1, #l do local u = l[i] ' .. body .. ' end')
    local t = (clock() - t0) * 1e6
    if t < b then b = t end
  end
  return b
end
local out = { counts = counts }
for _, class in ipairs({ 'moving', 'halted', 'idle_same_type' }) do
  local n = tonumber((net.dostring_in('server', 'return tostring(#__np_classes.' .. class .. ')'))) or 0
  local empty = best(class, '')
  local per = function(body)
    return n > 0 and (best(class, body) - empty) / n or 0
  end
  out[class] = {
    units = n,
    by_object_id = per('local x, y, z = DCS.getUnitPositionByObjectId(u.id_)'),
    get_point = per('local p = u:getPoint()'),
    get_position = per('local p = u:getPosition()'),
  }
end
net.dostring_in('server', '__np_classes = nil')
return out
