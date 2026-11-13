-- E31: leave the server from a client (net.stop_game), which takes the
-- player out of its aircraft and its slot. Run with
-- `cargo xtask eval <client-target> hook`.
-- states: hook
-- timeout: 20

local E = ...
E.observe('stop_game', E.pack(net.stop_game()), { info = true })
return 'done'
