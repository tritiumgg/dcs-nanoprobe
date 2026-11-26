-- Catalog: marks and drawings added and removed by script.
-- states: server
-- timeout: 60

local E = ...
local S = E.lib('scenario')
local R = E.lib('recorder')
R.clear()
R.start()

local origin = S.origin()
local MARK, COALITION_MARK, LINE = 980001, 980002, 980003
local p1 = { x = origin.x + 500, y = origin.y, z = origin.z + 500 }
local p2 = { x = origin.x + 1500, y = origin.y, z = origin.z + 1500 }

local step, mark = 1, timer.getTime()
local function after(seconds)
  return timer.getTime() - mark >= seconds
end
local function next_step(cause)
  step, mark = step + 1, timer.getTime()
  R.mark(cause)
end

next_step('mark-add')
trigger.action.markToAll(MARK, 'np mark', p1, true)
trigger.action.markToCoalition(COALITION_MARK, 'np coalition mark', p2, coalition.side.BLUE, true)

return E.wait(function()
  if step == 2 and after(3) then
    next_step('line-add')
    trigger.action.lineToAll(-1, LINE, p1, p2, { 1, 0, 0, 1 }, 1, true)
  elseif step == 3 and after(3) then
    next_step('mark-remove')
    trigger.action.removeMark(MARK)
    trigger.action.removeMark(COALITION_MARK)
    trigger.action.removeMark(LINE)
  elseif step == 4 and after(3) then
    -- The mark events' time against model time (spec/records.md).
    local entry = R.report().by['mark-add'] and R.report().by['mark-add'].S_EVENT_MARK_ADDED
    local sample = entry and entry.samples[1]
    E.observe('fact.mark_added_time', sample and sample.time, { info = true })
    E.observe('fact.model_time', timer.getTime(), { info = true })
    E.observe('catalog', R.report(), { info = true })
    -- Nothing of this scenario is left: its marks are removed above.
    S.clear()
    return 'done'
  end
  return nil
end, 50, 'running')
