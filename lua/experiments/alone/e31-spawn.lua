-- E31: put a client that has taken a slot into its aircraft, as ED's "Fly"
-- button does (net.spawn_player), and report the unit it controls after
-- 15 s. Run with `cargo xtask eval <client-target> hook` after
-- e30-take-slot.lua.
-- states: hook
-- timeout: 30

local E = ...
E.observe('spawn_player', E.pack(net.spawn_player()), { info = true })
local t0 = DCS.getRealTime()
return E.wait(function()
  if DCS.getRealTime() - t0 < 15 then
    return nil
  end
  E.observe('player_unit', tostring(DCS.getPlayerUnit()), { info = true })
  return 'done'
end, 25)
