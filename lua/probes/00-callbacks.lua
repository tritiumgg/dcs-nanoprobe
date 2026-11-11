-- The hook callbacks DCS has made since the process started.
-- states: hook

local E = ...
local counts = E.hook.counts()

E.observe('frames_seen', counts.frames > 0, { windows = true })
-- No onSimulationFrame arrives during a mission load (LOG).
E.observe('frames_during_load', counts.frames_loading, { windows = 0 })
-- onMissionLoadProgress never fired (LOG).
E.observe('load_progress_calls', counts.onMissionLoadProgress or 0, { windows = 0 })
E.observe('counts', counts, { info = true })
E.observe('paused', DCS.getPause(), { info = true })
E.observe('model_time', DCS.getModelTime(), { info = true })
