-- E13: spawn 2,000 ground units while paused, in one frame, as the restore
-- burst would. The rule: it completes; the time is recorded.
-- states: hook
-- timeout: 180

local E = ...
local agent = E.lib('agent').open()

local function cross(state, source)
  local ok, value = E.cross(state, source)
  if not ok then
    error(state .. ': ' .. tostring(value))
  end
  return value
end

local was_paused = DCS.getPause()
local step, gone_since = 'pause', nil

return E.wait(function()
  if step == 'pause' then
    DCS.setPause(true)
    step = 'spawn'
    return nil
  end
  if step == 'spawn' then
    if not DCS.getPause() then
      return nil
    end
    local model = DCS.getModelTime()
    local t0 = agent.clock()
    local result = cross('server', [[
      local S = E.lib('scenario')
      local agent = E.lib('agent').open()
      local t0 = agent.clock()
      local names, costs = S.spawn_grid(2000, { label = 'e13', offset = 3000 })
      local total = agent.clock() - t0
      table.sort(costs)
      local units = 0
      for _, name in ipairs(names) do
        local group = Group.getByName(name)
        units = units + (group and group:getSize() or 0)
      end
      E.track.global('np_e13', names)
      return string.format('%d %d %.3f %.3f %.3f', #names, units, total * 1000,
        costs[math.ceil(#costs / 2)] * 1000, costs[#costs] * 1000)
    ]])
    local frame_ms = (agent.clock() - t0) * 1000
    local groups, units, total, p50, max = result:match('(%d+) (%d+) (%S+) (%S+) (%S+)')
    E.observe('groups', tonumber(groups), { windows = 200 })
    E.observe('units', tonumber(units), { windows = 2000 })
    E.observe('total_ms', tonumber(total), { timing = true })
    E.observe('per_group_ms', { p50 = tonumber(p50), max = tonumber(max) }, { timing = true })
    E.observe('frame_ms', frame_ms, { timing = true })
    E.observe('still_paused', DCS.getPause(), { windows = true })
    E.observe('model_time_still', DCS.getModelTime() == model, { windows = true })
    cross('server', "for _, name in ipairs(np_e13) do E.lib('scenario').destroy(name) end return 'ok'")
    step = 'remove'
    gone_since = agent.clock()
    DCS.setPause(false)
    return nil
  end
  if DCS.getPause() then
    DCS.setPause(false)
    return nil
  end
  if cross('server', "for _, n in ipairs(np_e13) do if Group.getByName(n) then return 'left' end end return 'gone'")
    ~= 'gone' then
    return nil
  end
  E.observe('seconds_until_removed', agent.clock() - gone_since, { info = true })
  DCS.setPause(was_paused)
  return 'done'
end, 170)
