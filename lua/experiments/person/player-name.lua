-- Person tests: the player names the server sees, from net.get_player_list
-- and net.get_player_info(id, 'name'), with each player's side and slot. It
-- reads no ucid and no address.
-- states: hook
-- timeout: 10

local E = ...
local rows = {}
for _, id in ipairs(net.get_player_list() or {}) do
  rows[#rows + 1] = string.format('id=%s name=%s side=%s slot=%s', tostring(id),
    tostring(net.get_player_info(id, 'name')), tostring(net.get_player_info(id, 'side')),
    tostring(net.get_player_info(id, 'slot')))
end
E.observe('players', rows, { info = true })
return #rows
