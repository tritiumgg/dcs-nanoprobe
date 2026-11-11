-- Crash capture, part one: a deliberate panic in the server state's agent.
-- The entry catches it, the agent is disabled for the rest of the process,
-- and agent.log holds the panic with its location and breadcrumb. The rule:
-- a diagnosable bundle. Run last: the agent stays disabled until DCS
-- restarts.
-- states: server

local E = ...
local agent = E.lib('agent').open()
agent.crumb(90, 'E90 deliberate panic')
local n = select('#', agent.panic())
local status = agent.status()
E.observe('panic_returned', n, { windows = 0 })
E.observe('disabled', status.disabled, { windows = true })
E.observe('reason', status.reason, { info = true })
E.observe('values_after', select('#', agent.thread()), { windows = 0 })
return 'done'
