-- E30: take a slot from Lua, as ED's role screen does
-- (Scripts/UI/MultiplayerSelectRoleMap: net.set_coalition, then net.set_slot).
-- Run with `cargo xtask eval <target> hook` on a client that has joined a
-- server and not taken a slot. It takes the first slot the client is offered,
-- in any coalition, so it works with any mission that has player slots.
-- states: hook
-- timeout: 60

local E = ...
local SIDES = { red = 1, blue = 2 }

local target, side
for name, id in pairs(SIDES) do
  local slot = (DCS.getAvailableSlots(name) or {})[1]
  if slot and not target then
    target, side = slot, id
  end
end
if not target then
  error('the client is offered no slot')
end
E.observe('slot', { side = side, unitId = target.unitId, type = target.type }, { info = true })
E.observe('set_coalition', E.pack(net.set_coalition(side, '')), { info = true })
E.observe('set_slot', E.pack(net.set_slot(side, target.unitId, '', nil)), { info = true })

local start = DCS.getRealTime()
return E.wait(function()
  local id = net.get_my_player_id()
  local side, slot = net.get_slot(id)
  if tostring(slot) ~= tostring(target.unitId) and DCS.getRealTime() - start < 20 then
    return nil
  end
  E.observe('get_slot', { side = side, slot = tostring(slot) }, { info = true })
  E.observe('player_unit', tostring(DCS.getPlayerUnit()), { info = true })
  E.observe('took_slot', tostring(slot) == tostring(target.unitId), { windows = true })
  return 'done'
end, 30)
