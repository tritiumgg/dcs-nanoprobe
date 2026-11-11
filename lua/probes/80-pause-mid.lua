-- The pause matrix (E17) in a mid-mission pause: resume, let model time run
-- for two seconds, pause with DCS.setPause(true), then run the matrix. The
-- simulation is left paused.
-- states: hook
-- timeout: 90

local E = ...
local M = E.lib('pause')
local phase, since, step, asked = 'resume', nil, nil, false

return E.wait(function()
  if step then
    return step()
  end
  if phase == 'resume' then
    if DCS.getPause() then
      if not asked then
        DCS.setPause(false)
        asked = true
      end
      return nil
    end
    phase, since, asked = 'run', DCS.getModelTime(), false
    return nil
  end
  if phase == 'run' then
    if DCS.getPause() then
      -- Something else paused it (see 81-pause-idle); resume and go on.
      phase = 'resume'
      return nil
    end
    if DCS.getModelTime() - since < 2 then
      return nil
    end
    DCS.setPause(true)
    phase = 'pausing'
    return nil
  end
  -- DCS.setPause takes effect on a later frame (LIVE).
  if not DCS.getPause() then
    return nil
  end
  step = M.start(E, 'mid')
  return nil
end, 80)
