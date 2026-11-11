-- World events in the server state: what a spawn, a scripted removal and a
-- death by explosion emit, and what an event carries.
-- states: server
-- timeout: 90

local E = ...
local S = E.lib('scenario')
local R = E.lib('recorder')
R.clear()
R.start()

local step, mark = 1, timer.getTime()
local function after(seconds)
  return timer.getTime() - mark >= seconds
end
local function next_step()
  step, mark = step + 1, timer.getTime()
end

local origin = S.origin()
R.mark('spawn')
local units = {}
for i = 1, 4 do
  units[i] = { name = 'np-ev-' .. i, x = origin.x + 400 + i * 15, z = origin.z - 300 }
end
local group = S.ground('np-ev', 'M-113', units)

return E.wait(function()
  if step == 1 then
    if R.count('spawn', 'S_EVENT_BIRTH') < 4 and not after(3) then
      return nil
    end
    -- Every spawned unit emits S_EVENT_BIRTH (LIVE).
    E.observe('births', R.count('spawn', 'S_EVENT_BIRTH'), { windows = 4 })
    -- An event with an initiator carries initiator_data, a plain table
    -- (LIVE).
    E.observe('initiator_data_keys', R.keys('spawn', 'S_EVENT_BIRTH', 'initiator_data'), { windows = {
      'coalition', 'mission_id', 'object_category', 'object_id', 'unit_name', 'unit_type',
    } })
    R.mark('destroy')
    group:destroy()
    -- Group:destroy() is deferred: the group still exists within the call
    -- (LIVE).
    E.observe('exists_right_after_destroy', group:isExist(), { windows = true })
    next_step()
  elseif step == 2 then
    if not after(1.5) then
      return nil
    end
    -- A scripted removal emits no event (LIVE).
    E.observe('events_from_destroy', R.count_all('destroy'), { windows = 0 })
    E.observe('gone_after_destroy', Group.getByName('np-ev') == nil, { windows = true })
    R.mark('lone')
    S.ground('np-lone', 'M-113', { { name = 'np-lone-1', x = origin.x + 400, z = origin.z - 600 } })
    next_step()
  elseif step == 3 then
    if not after(1) then
      return nil
    end
    R.mark('explosion')
    S.kill('np-lone-1', 500)
    next_step()
  elseif step == 4 then
    if R.count('explosion', 'S_EVENT_DEAD') < 1 and not after(15) then
      return nil
    end
    -- A unit killed by an explosion emits S_EVENT_UNIT_LOST and
    -- S_EVENT_DEAD, and no S_EVENT_KILL, because there is no killer (LIVE).
    E.observe('explosion.dead', R.count('explosion', 'S_EVENT_DEAD'), { windows = 1 })
    E.observe('explosion.unit_lost', R.count('explosion', 'S_EVENT_UNIT_LOST'), { windows = 1 })
    E.observe('explosion.kill', R.count('explosion', 'S_EVENT_KILL'), { windows = 0 })
    E.observe('explosion.ids', R.ids('explosion'), { info = true })
    E.observe('recorded', R.report(), { info = true })
    S.destroy('np-lone')
    return 'done'
  end
  return nil
end, 60, 'running')
