-- S16, E35: take a slot and ask to spawn at once, with nothing waited between
-- the two calls, so the server's `MODERATE SLOT` (sent when the `SLOT` record
-- arrives) lands before, between or after them. Run with
-- `cargo xtask eval <client-target> hook` on a client that has joined a server.
-- It takes the first slot the client is offered, as e30-take-slot.lua does, then
-- calls net.spawn_player() in the same step, and 4 s later reports the slot and
-- the unit the client has. Every call's return is recorded.
-- states: hook
-- timeout: 30

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
E.observe('spawn_player', E.pack(net.spawn_player()), { info = true })

local start = DCS.getRealTime()
return E.wait(function()
  if DCS.getRealTime() - start < 4 then
    return nil
  end
  local id = net.get_my_player_id()
  local got_side, got_slot = net.get_slot(id)
  E.observe('get_slot', { side = got_side, slot = tostring(got_slot) }, { info = true })
  E.observe('player_unit', tostring(DCS.getPlayerUnit()), { info = true })
  return 'done'
end, 20)
