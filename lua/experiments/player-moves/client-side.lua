-- Player moves, the client's half of the race step: the client changes its
-- own coalition, as the role screen does (net.set_coalition), while the
-- server's moves.lua waits for that change and answers it with a move to the
-- side it read before. Run with `cargo xtask eval <client> hook` on a client
-- that has joined and holds no slot. SIDE is filled in by the caller.
-- Rule: recorded as read.
-- states: hook
-- timeout: 60

local E = ...
local SIDE = tonumber('<side>') or 1

local me = net.get_my_player_id()
E.observe('before', { side = net.get_player_info(me, 'side'), slot = tostring(net.get_player_info(me, 'slot')) },
  { info = true })
E.observe('set_coalition', E.pack(pcall(net.set_coalition, SIDE, '')), { info = true })

local start = DCS.getRealTime()
local seen = {}
return E.wait(function()
  local side = net.get_player_info(me, 'side')
  if seen[#seen] ~= side then
    seen[#seen + 1] = side
    E.observe('side_' .. #seen, { side = side, at = DCS.getRealTime() - start }, { info = true })
  end
  if DCS.getRealTime() - start < 15 then
    return nil
  end
  return 'done'
end, 30)
