-- Radio transmission: stop every case case.lua can start.
-- Returns {stopped, errors}.
-- State: server (the mission scripting state).
-- Evidence: trigger.action.stopRadioTransmission(name) is PRIOR; that it
-- raises nothing for a name it does not hold was seen in run 4 (APPLIED).
-- Run: cargo xtask eval <target> server lua/experiments/radio-transmission/stop.lua
-- states: server
-- timeout: 30
local E = ...
local out = { stopped = 0, errors = {} }
for i = 1, 16 do
  local ok, err = pcall(trigger.action.stopRadioTransmission, 'np-rt-c' .. i)
  if ok then
    out.stopped = out.stopped + 1
  else
    out.errors[#out.errors + 1] = tostring(err)
  end
end
return out
