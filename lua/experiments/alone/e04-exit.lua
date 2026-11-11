-- E4, second half: a clean exit with the agent DLL loaded in the hook state
-- and opened in the server state. Run with `cargo xtask eval <target> hook` on
-- a server with no players. DCS exits, and the image's AUTOSTART starts it
-- again. What decides the result is the previous run's log: an orderly
-- shutdown, and no crash report.
--
-- DCS.exitProcess() in the hook state did nothing on the dedicated server
-- (LIVE), so the gui state's is called as well.
-- states: hook

local E = ...
DCS.exitProcess()
local ok, value = E.cross('gui', 'DCS.exitProcess() return "called"')
return 'exit requested; gui: ' .. tostring(ok) .. ' ' .. tostring(value)
