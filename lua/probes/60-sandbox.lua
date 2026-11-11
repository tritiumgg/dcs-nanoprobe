-- The sandboxed mission scripting environment can reach the unsandboxed
-- server state when neither gate key is set.
-- states: sse

local E = ...
local dostring = type(net) == 'table' and net.dostring_in
local r = E.pack(pcall(dostring, 'server', 'return tostring(_G)'))

-- A mission script escaped the sandbox on an unconfigured install (LIVE).
E.observe('reaches_server_state', r[1] == true and type(r[2]) == 'string' and r[2]:match('^table') ~= nil,
  { windows = true })
E.observe('reply_types', { type(r[2]), type(r[3]) }, { info = true })
