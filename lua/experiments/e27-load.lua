-- E27: the static-CRT MSVC DLL loads in the real prefix: the hook opened it
-- at start, and it opens in the server state too. The rule: loads; otherwise
-- try the dynamic CRT.
-- states: hook server

local E = ...
local agent = E.lib('agent').open()
local status = agent.status()
E.observe('loaded', type(agent) == 'table', { windows = true })
-- The ABI of the agent this repository builds (crates/agent/src/lib.rs).
E.observe('abi', agent.abi, { windows = '10' })
-- The host-native build under the mock does not pin itself. It carries the
-- name of the file it was opened from, as the DLL does.
E.observe('pinned', status.pinned, { windows = true, mock = false })
E.observe('module_matches_build', status.module == 'nanoprobe_agent_' .. agent.build .. '.dll',
  { windows = true, mock = true })
E.observe('pdb', status.pdb, { info = true })
E.observe('opens', status.opens, { info = true })
E.observe('disabled', status.disabled, { windows = false })
return 'done'
