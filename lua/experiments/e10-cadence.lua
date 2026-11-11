-- E10: frame and timer cadence on the server, running and paused, and what
-- the server offers for time acceleration. Recorded; no pass rule.
-- states: hook
-- timeout: 90

local E = ...
local A = E.lib('agent')
local agent = A.open()

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

-- What the hook and server states offer for time acceleration.
local names = {}
for _, t in ipairs({ { 'DCS', DCS }, { 'net', net }, { 'Export', Export } }) do
  for key in pairs(t[2]) do
    if tostring(key):lower():find('accel') then
      names[#names + 1] = t[1] .. '.' .. key
    end
  end
end
table.sort(names)
E.observe('acceleration_names_hook', names, { info = true })
E.observe('acceleration_names_server', cross('server', [[
  local found = {}
  for _, t in ipairs({ { 'timer', timer }, { 'env', env }, { 'trigger.action', trigger.action } }) do
    for key in pairs(t[2]) do
      if tostring(key):lower():find('accel') then found[#found + 1] = t[1] .. '.' .. key end
    end
  end
  table.sort(found)
  return table.concat(found, ' ')
]]), { info = true })
local ok, factor = pcall(Export.LoGetModelTimeAcceleration)
E.observe('model_time_acceleration', ok and factor or tostring(factor), { info = true })

local was_paused = DCS.getPause()
local phase, deltas, last, phase_end = 'running', { running = {}, paused = {} }, nil, nil
local model_start, real_start

local TIMER = [[
  local agent = E.lib('agent').open()
  E.track.global('np_e10', { model = {}, real = {} })
  local last_model, last_real
  local stop = timer.getTime() + 3
  timer.scheduleFunction(function(_, t)
    local now_real = agent.clock()
    if last_model then
      np_e10.model[#np_e10.model + 1] = t - last_model
      np_e10.real[#np_e10.real + 1] = now_real - last_real
    end
    last_model, last_real = t, now_real
    if t > stop then
      np_e10.done = true
      return nil
    end
    return t + 0.001
  end, nil, timer.getTime() + 0.001)
  return 'armed'
]]

local function summary(list, scale)
  local s = A.stats(list)
  local out = {}
  for k, v in pairs(s) do
    out[k] = k == 'n' and v or v * scale
  end
  return out
end

return E.wait(function()
  -- Phases are timed on DCS's real time; frame intervals on the agent's
  -- clock.
  local real = DCS.getRealTime()
  local now = agent.clock()
  if phase == 'running' then
    if DCS.getPause() then
      DCS.setPause(false)
      last = nil
      return nil
    end
    if not phase_end then
      phase_end = real + 10
      model_start, real_start = DCS.getModelTime(), real
      cross('server', TIMER)
    end
    if last then
      deltas.running[#deltas.running + 1] = now - last
    end
    last = now
    if real < phase_end then
      return nil
    end
    E.observe('model_per_real', (DCS.getModelTime() - model_start) / (real - real_start), { info = true })
    phase, phase_end, last = 'paused', nil, nil
    DCS.setPause(true)
    return nil
  end
  if not DCS.getPause() then
    return nil
  end
  phase_end = phase_end or real + 5
  if last then
    deltas.paused[#deltas.paused + 1] = now - last
  end
  last = now
  if real < phase_end then
    return nil
  end
  DCS.setPause(was_paused)
  E.observe('frame_ms.running', summary(deltas.running, 1000), { info = true })
  E.observe('frame_ms.paused', summary(deltas.paused, 1000), { info = true })
  E.observe('timer_model_ms', cross('server', [[
    local list = np_e10.model
    table.sort(list)
    return string.format('n=%d min=%.3f p50=%.3f max=%.3f', #list, list[1] * 1000,
      list[math.ceil(#list / 2)] * 1000, list[#list] * 1000)
  ]]), { info = true })
  E.observe('timer_real_ms', cross('server', [[
    local list = np_e10.real
    table.sort(list)
    return string.format('n=%d min=%.3f p50=%.3f max=%.3f', #list, list[1] * 1000,
      list[math.ceil(#list / 2)] * 1000, list[#list] * 1000)
  ]]), { info = true })
  return 'done'
end, 80)
