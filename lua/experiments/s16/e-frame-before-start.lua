-- Does DCS run a simulation frame before onSimulationStart? Rule: the counts
-- of `onSimulationFrame` calls between onMissionLoadBegin and onMissionLoadEnd,
-- between onMissionLoadEnd and onSimulationStart, and in the 5 frames after the
-- start, over one load of the running mission, with the pause state and the
-- model time at each. A frame before the start means the agent's pump can run
-- before the simulation exists.
--
-- It loads the running mission itself (net.load_mission), so run it on a
-- server with no player. It adds a second callback set (DCS.setUserCallbacks is
-- additive and has no removal), which does nothing once the snippet is done or
-- reset. Run: `cargo xtask eval <target> hook lua/experiments/s16/e-frame-before-start.lua`.
-- states: hook
-- timeout: 300

local E = ...
local file = DCS.getMissionFilename()
if type(file) ~= 'string' then
  error('no mission file to load')
end

local live = true
local phase = 'idle'
local frames = { loading = 0, after_end = 0, after_start = 0 }
local samples = {}
local order = {}

local function try(fn)
  local ok, value = pcall(fn)
  return ok and value or nil
end

local function sample(kind)
  if #samples < 24 then
    samples[#samples + 1] = {
      phase = kind,
      model = try(DCS.getModelTime),
      real = try(DCS.getRealTime),
      paused = try(DCS.getPause),
    }
  end
end

local callbacks = {
  onMissionLoadBegin = function()
    if live then
      phase = 'loading'
      order[#order + 1] = 'onMissionLoadBegin'
    end
  end,
  onMissionLoadEnd = function()
    if live and phase == 'loading' then
      phase = 'after_end'
      order[#order + 1] = 'onMissionLoadEnd'
    end
  end,
  onSimulationStart = function()
    if live and phase ~= 'idle' then
      phase = 'after_start'
      order[#order + 1] = 'onSimulationStart'
    end
  end,
  onSimulationFrame = function()
    if live and frames[phase] then
      frames[phase] = frames[phase] + 1
      if phase ~= 'after_start' or frames.after_start <= 5 then
        sample(phase)
      end
    end
  end,
}
DCS.setUserCallbacks(callbacks)
E.track.defer('frame counter off', function()
  live = false
end)

E.observe('mission_file_is_string', true, { info = true })
E.observe('net_load_mission', tostring(net.load_mission(file)), { info = true })

return E.wait(function()
  if phase ~= 'after_start' or frames.after_start < 5 then
    return nil
  end
  live = false
  E.observe('order', table.concat(order, ' '), { info = true })
  E.observe('frames_between_begin_and_end', frames.loading, { info = true })
  E.observe('frames_between_end_and_start', frames.after_end, { info = true })
  E.observe('frames_after_start', frames.after_start, { info = true })
  E.observe('frame_before_start', frames.loading + frames.after_end > 0, { info = true })
  E.observe('samples', samples, { info = true })
  return 'done'
end, 280)
