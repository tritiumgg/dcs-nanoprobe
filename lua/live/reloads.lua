-- Reload the running mission RELOADS times, one after another, each once the
-- last has started. Run by `cargo xtask link` in the hook state through the
-- eval channel; the sidecar side of the test times what the agent sends.
-- `cargo xtask link` sets RELOADS and allows each load 300 s, a limit and
-- not an expectation.
-- states: hook

local E = ...
local RELOADS = 10

local file = DCS.getMissionFilename()
if type(file) ~= 'string' then
  error('no mission file to reload')
end

local done, seq, started_at = 0, nil, nil
local seconds = {}

return E.wait(function()
  if seq then
    local started = false
    for _, entry in ipairs(E.hook.since(seq)) do
      started = started or entry.name == 'onSimulationStart'
    end
    if not started then
      return nil
    end
    seconds[#seconds + 1] = DCS.getRealTime() - started_at
    done = done + 1
    seq = nil
  end
  if done < RELOADS then
    seq = E.hook.seq()
    started_at = DCS.getRealTime()
    net.load_mission(file)
    return nil
  end
  E.observe('reloads', done, { info = true })
  E.observe('load_seconds', seconds, { info = true })
  return 'reloaded ' .. done
end, RELOADS * 300)
