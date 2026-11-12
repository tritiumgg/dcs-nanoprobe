-- E18, part two: reload the running mission while a client is connected.
-- Run with `cargo xtask eval <server target> hook`. Records the callbacks the
-- server's hook sees until the simulation starts again; the player reports
-- what the client did.
-- states: hook
-- timeout: 300

local E = ...
local file = DCS.getMissionFilename()
local seq = E.hook.seq()
local t0 = DCS.getRealTime()
E.observe('load_mission', E.pack(net.load_mission(file)), { info = true })
return E.wait(function()
  local names, started = {}, false
  for _, entry in ipairs(E.hook.since(seq)) do
    names[#names + 1] = entry.name
    started = started or entry.name == 'onSimulationStart'
  end
  if not started then
    return nil
  end
  E.observe('callbacks', names, { info = true })
  E.observe('seconds', DCS.getRealTime() - t0, { info = true })
  return 'reloaded'
end, 280)
