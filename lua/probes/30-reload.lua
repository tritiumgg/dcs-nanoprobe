-- A mission reload through net.load_mission: the callbacks it makes, and
-- what survives it in each state.
-- states: hook
-- timeout: 300

local E = ...

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

local file = DCS.getMissionFilename()
if type(file) ~= 'string' then
  error('no mission file to reload')
end

-- Before: a global, an event handler and a timer in the server state, and
-- each state's identity.
local before = {
  server = cross('server', [[
    E.track.global('np_reload_mark', 7)
    E.track.global('np_reload_events', {})
    E.track.global('np_reload_timer', 0)
    E.track.handler({ onEvent = function(_, event)
      np_reload_events[#np_reload_events + 1] = event.id
    end })
    E.track.timer(function() _G.np_reload_timer = 1 end, nil, timer.getTime() + 3)
    return tostring(_G)
  ]]),
  model = tonumber(cross('server', 'return timer.getTime()')),
  mission = cross('mission', 'return tostring(_G)'),
  sse = cross('sse', 'return tostring(_G)'),
  export = cross('export', 'return tostring(_G)'),
}
local seq = E.hook.seq()
local counts = E.hook.counts()

local returned = E.pack(net.load_mission(file))
-- It returned 0 for a mission that then loaded (LIVE under Wine).
E.observe('load_mission_returns', { n = returned.n, first = returned[1] }, { info = true })

local started, resumed

return E.wait(function()
  if not started then
    for _, entry in ipairs(E.hook.since(seq)) do
      started = started or entry.name == 'onSimulationStart'
    end
    if not started then
      return nil
    end
    E.observe('paused_after_load', DCS.getPause(), { info = true })
  end
  -- A server with no client starts the mission paused; the rest of this
  -- probe and the probes after it need model time.
  if DCS.getPause() then
    if not resumed then
      resumed = true
      DCS.setPause(false)
    end
    return nil
  end
  -- Wait until the old timer would have run, had the load kept it.
  if DCS.getModelTime() < before.model + 5 then
    return nil
  end

  local order, theatre = {}, {}
  for _, entry in ipairs(E.hook.since(seq)) do
    if entry.name ~= 'onGameEvent' and entry.name ~= 'onSimulationResume' and entry.name ~= 'onSimulationPause'
      and not entry.name:find('^onNet[CD]') and not entry.name:find('^onPlayer[CD]') and #order < 12 then
      order[#order + 1] = entry.name
    end
    theatre[entry.name] = theatre[entry.name] or type(entry.theatre)
    if entry.name == 'onSimulationStart' then
      break
    end
  end
  -- The order on a client hosting the session, and on a dedicated server,
  -- whose own player also stops (LIVE on both).
  E.observe('callback_order', order, {
    windows = {
      'onSimulationStop', 'onNetMissionChanged', 'onMissionLoadBegin', 'onPlayerStart', 'onMissionLoadEnd',
      'onSimulationStart',
    },
    wine = {
      'onSimulationStop', 'onPlayerStop', 'onNetMissionChanged', 'onMissionLoadBegin', 'onPlayerStart',
      'onMissionLoadEnd', 'onSimulationStart',
    },
  })
  local now = E.hook.counts()
  -- No onSimulationFrame arrives during a load, and onMissionLoadProgress
  -- never fires (LOG).
  E.observe('frames_during_load', now.frames_loading - counts.frames_loading, { windows = 0 })
  E.observe('load_progress_calls', (now.onMissionLoadProgress or 0) - (counts.onMissionLoadProgress or 0),
    { windows = 0 })
  -- The theater is readable at onMissionLoadBegin and nil at
  -- onSimulationStop (PRIOR).
  E.observe('theatre_at_load_begin', theatre.onMissionLoadBegin, { windows = 'string' })
  E.observe('theatre_at_stop', theatre.onSimulationStop, { windows = 'nil' })

  -- The server state and the mission state are the same states after a
  -- reload; the sandbox and the export state are made again (LIVE).
  E.observe('server_state_kept', cross('server', 'return tostring(_G)') == before.server, { windows = true })
  E.observe('server_global_kept', cross('server', 'return tostring(np_reload_mark)'), { windows = '7' })
  E.observe('mission_state_kept', cross('mission', 'return tostring(_G)') == before.mission, { windows = true })
  E.observe('sandbox_made_again', cross('sse', 'return tostring(_G)') ~= before.sse, { windows = true })
  E.observe('export_made_again', cross('export', 'return tostring(_G)') ~= before.export, { windows = true })

  -- The handler is still registered and saw the mission end (12) and the
  -- next one start (11); the timer was cleared; model time went back to 0
  -- (LIVE).
  local events = cross('server', [[return ',' .. table.concat(np_reload_events, ',') .. ',']])
  E.observe('handler_saw_mission_end', events:find(',12,', 1, true) ~= nil, { windows = true })
  E.observe('handler_saw_mission_start', events:find(',11,', 1, true) ~= nil, { windows = true })
  E.observe('events_seen', events, { info = true })
  E.observe('timer_from_before_ran', cross('server', 'return tostring(np_reload_timer)'), { windows = '0' })
  E.observe('model_time_restarted', DCS.getModelTime() < before.model + 5 + 60, { info = true })
  E.observe('model_time_before', before.model, { info = true })
  return 'reloaded'
end, 280, 'running')
