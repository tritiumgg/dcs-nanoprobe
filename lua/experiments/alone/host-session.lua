-- Host a private multiplayer session on a DCS client, from its main menu, with
-- the shell mission the runner uploaded to nanoprobe/dev/missions/np-shell.miz.
-- Run with `cargo xtask eval <target> gui`. A client at its main menu has no
-- mission and no `server` state; hosting gives it both.
--
-- It starts from ED's own defaults (`net.get_default_server_settings()`, as
-- ED's multiplayer screen does, MissionEditor/modules/mul_create_server.lua)
-- and never reads the profile's serverSettings.lua. The session is not public,
-- and its password is random and not recorded, so no one can join it.
-- `net.stop_game()` ends it.
--
-- It listens on port 10318, not DCS's default 10308: a real server elsewhere on
-- the network may own 10308, and a host may ask the router to forward its port
-- (UPnP).
-- states: gui

local E = ...
local mission = lfs.writedir() .. 'nanoprobe/dev/missions/np-shell.miz'
local file = io.open(mission, 'rb')
if not file then
  error('no shell mission at ' .. mission)
end
file:close()

local settings = net.get_default_server_settings()
local seed = tostring({}) .. tostring(os.clock()) .. tostring(os.time())
local password = {}
for i = 1, #seed do
  password[#password + 1] = string.format('%02x', (seed:byte(i) * 31 + i * 17) % 256)
end
settings.name = 'nanoprobe test session'
settings.description = 'private test session; no one can join'
settings.password = table.concat(password)
settings.isPublic = false
settings.maxPlayers = 2
settings.port = 10318
settings.missionList = { mission }
settings.listStartIndex = 1

E.observe('default_keys', (function()
  local keys = {}
  for k in pairs(net.get_default_server_settings()) do
    keys[#keys + 1] = tostring(k)
  end
  table.sort(keys)
  return keys
end)(), { info = true })
local result = E.pack(net.start_server(settings))
E.observe('port', settings.port, { info = true })
E.observe('start_server', { n = result.n, first = tostring(result[1]) }, { info = true })
return 'requested'
