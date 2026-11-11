-- Resume the simulation, so the probes that need model time can run. The
-- last probe pauses it again if it was paused here.
-- states: hook
-- timeout: 30

local E = ...
local was = DCS.getPause()
E.store.was_paused = was
E.observe('paused_before', was, { info = true })
if not was then
  return 'running'
end

local seq = E.hook.seq()
local frames = E.hook.counts().frames
DCS.setPause(false)
-- DCS.setPause takes effect on a later frame (LIVE).
E.observe('resumed_within_the_call', DCS.getPause() == false, { windows = false })

return E.wait(function()
  if DCS.getPause() then
    return nil
  end
  E.observe('frames_until_resumed', E.hook.counts().frames - frames, { info = true })
  local resume = false
  for _, entry in ipairs(E.hook.since(seq)) do
    resume = resume or entry.name == 'onSimulationResume'
  end
  E.observe('resume_callback_seen', resume, { info = true })
  return 'resumed'
end, 15)
