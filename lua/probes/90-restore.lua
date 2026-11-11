-- Pause the simulation again if the suite found it paused.
-- states: hook
-- timeout: 30

local E = ...
E.observe('counts', E.hook.counts(), { info = true })
if not E.store.was_paused or DCS.getPause() then
  return 'left as it is'
end
DCS.setPause(true)
return E.wait(function()
  if not DCS.getPause() then
    return nil
  end
  return 'paused again'
end, 10)
