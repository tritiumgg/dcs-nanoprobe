-- Person tests: play the packed tone `np-tone.wav` (0.5 s, 440 Hz) three
-- times, 1 s apart, with trigger.action's coalition form. The person says whether
-- they heard it. Rule: heard or not, recorded as the person reports.
-- states: server
-- timeout: 20

local E = ...
local FORM = 'coalition'
local FILE = 'np-tone.wav'
local unit
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  unit = unit or (coalition.getPlayers(side) or {})[1]
end
if not unit then
  error('no player in an aircraft')
end
local function play()
  if FORM == 'all' then
    trigger.action.outSound(FILE)
  elseif FORM == 'coalition' then
    trigger.action.outSoundForCoalition(unit:getCoalition(), FILE)
  else
    trigger.action.outSoundForGroup(unit:getGroup():getID(), FILE)
  end
end
local plays, started = 0, timer.getTime()
return E.wait(function()
  local due = math.floor(timer.getTime() - started) + 1
  while plays < due and plays < 3 do
    play()
    plays = plays + 1
  end
  if plays >= 3 then
    return FORM .. ' played 3 times, coalition ' .. unit:getCoalition() .. ', group ' .. unit:getGroup():getID()
  end
end, 15, 'running')
