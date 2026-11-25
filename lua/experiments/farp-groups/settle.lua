-- FARP groups, after `run.lua`: let the simulation run 10 s of model time and
-- pause, so the explosion's death and the destroys DCS holds while paused
-- take effect. The eval hook keeps a waiting snippet's mission paused unless
-- it waits with `running`, which `run.lua` did not; in run 1 the mission ran
-- about 2 s there. Run with `cargo xtask eval <target> hook`.
-- Rule: recorded as read.
-- states: hook
-- timeout: 60

local E = ...
if #net.get_player_list() > 1 then
  error('players connected')
end
local t0 = DCS.getModelTime()
return E.wait(function()
  local now = DCS.getModelTime()
  if now >= t0 + 10 then
    DCS.setPause(true)
    return { from = t0, to = now, paused = true }
  end
end, 50, 'running')
