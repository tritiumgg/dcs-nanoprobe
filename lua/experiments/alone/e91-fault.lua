-- Crash capture, part two: a deliberate access violation in the server
-- state's agent. DCS crashes. Run with `cargo xtask eval <target> server` on
-- a server with no players; no result comes back. The crash handler appends
-- one record to logs/crash.log with the code, the module offsets of the
-- stack and the breadcrumb; `cargo xtask symbolize` turns the offsets into
-- source lines with the build's PDB.
-- states: server

local E = ...
local agent = E.lib('agent').open()
agent.crumb(91, 'E91 deliberate access violation')
agent.fault()
return 'not reached'
