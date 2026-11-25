-- Player moves: the client's own view of its slot, read after the server
-- moved it. Run with `cargo xtask eval <client> hook` on a joined client.
-- Rule: recorded as read.
-- states: hook
-- timeout: 30

local me = net.get_my_player_id()
local side, slot = net.get_slot(me)
return {
  side = side,
  slot = tostring(slot),
  info_side = net.get_player_info(me, 'side'),
  info_slot = tostring(net.get_player_info(me, 'slot')),
  player_unit = tostring(DCS.getPlayerUnit()),
}
