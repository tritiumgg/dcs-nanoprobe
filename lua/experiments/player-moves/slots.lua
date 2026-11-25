-- states: hook
local out = {}
for _, side in ipairs({ 'red', 'blue' }) do
  local list = DCS.getAvailableSlots(side) or {}
  local first = {}
  for i = 1, math.min(4, #list) do
    local s = list[i]
    first[i] = tostring(s.unitId) .. ' ' .. type(s.unitId) .. ' ' .. tostring(s.type) .. ' ' .. tostring(s.role)
  end
  out[side] = { n = #list, first = first }
end
return out
