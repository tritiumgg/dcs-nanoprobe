-- The pause matrix (E17) when the server pauses because its last client
-- left. Anything that opens the game port and closes it counts as a client,
-- and a monitor that checks the port does that about once a minute (LOG).
-- The probe resumes the simulation, waits for DCS to pause it, then runs the
-- matrix. The simulation is left paused.
-- states: hook
-- timeout: 200

local E = ...
local M = E.lib('pause')
-- Seconds of real time to wait for the server to pause by itself.
local LIMIT = 150
local start, seq, step = DCS.getRealTime(), E.hook.seq(), nil
local resumed, asked = false, false

return E.wait(function()
  if step then
    return step()
  end
  local now = DCS.getRealTime()
  if not resumed then
    if DCS.getPause() then
      if not asked then
        DCS.setPause(false)
        asked = true
      end
      return nil
    end
    resumed, start, seq = true, now, E.hook.seq()
    return nil
  end
  if not DCS.getPause() then
    if now - start < LIMIT then
      return nil
    end
    E.observe('paused_by_itself', false, { info = true })
    E.note('the server did not pause by itself within ' .. LIMIT .. ' s; nothing observed')
    return 'skipped'
  end
  E.observe('paused_by_itself', true, { info = true })
  E.observe('seconds_until_paused', now - start, { info = true })
  -- Which hook callbacks came with the pause.
  local names = {}
  for _, entry in ipairs(E.hook.since(seq)) do
    names[#names + 1] = entry.name
  end
  E.observe('callbacks_until_paused', names, { info = true })
  step = M.start(E, 'idle')
  return nil
end, 190)
