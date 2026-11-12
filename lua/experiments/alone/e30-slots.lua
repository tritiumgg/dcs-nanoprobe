-- E30: what a joined client offers a script on the coalition and slot screens.
-- Run with `cargo xtask eval <target> hook` while the client is connected and
-- has not taken a slot. Reads only.
-- states: hook

local E = ...
local function keys(t)
  local out = {}
  for k, v in pairs(t or {}) do
    out[#out + 1] = tostring(k) .. '=' .. (type(v) == 'table' and 'table' or tostring(v))
  end
  table.sort(out)
  return table.concat(out, ' ')
end
local result = {
  multiplayer = DCS.isMultiplayer(),
  my_id = net.get_my_player_id(),
  coalitions = keys(DCS.getAvailableCoalitions()),
}
for _, side in ipairs({ 'blue', 'red' }) do
  local slots = DCS.getAvailableSlots(side) or {}
  local list = {}
  for i, slot in ipairs(slots) do
    if i <= 6 then
      list[#list + 1] = keys(slot)
    end
  end
  result[side .. '_count'] = #slots
  result[side .. '_first'] = list
end
return result
