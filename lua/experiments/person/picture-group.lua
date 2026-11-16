-- Person tests: show the packed 8 by 8 picture `np-picture.png` (key
-- ResKey_Action_902), to the player's group for 10 s (`a_out_picture_g`).
-- The Mission Editor's fields (ED-CODE, MissionEditor/modules/me_trigrules.lua)
-- without the last one, picture effect: file, seconds, clear view, start
-- delay, horizontal and vertical alignment (1 center), size, size units (1 is
-- percent of the window), in the form the agent's PICTURE command sends
-- (lua/agent/hook.lua). The person says what they see. Rule: recorded as the
-- person reports.
-- states: hook
-- timeout: 20

local E = ...
local FORM, SECONDS, CLEARVIEW = 'group', 10, false
local ok, ids = E.cross('server', [[
local unit
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  unit = unit or (coalition.getPlayers(side) or {})[1]
end
if not unit then return 'none' end
return unit:getCoalition() .. ' ' .. unit:getGroup():getID()
]])
if not ok then error('server: ' .. tostring(ids)) end
local side, gid = tostring(ids):match('(%d+) (%d+)')
if FORM ~= 'all' and FORM ~= 'stop' and not gid then error('no player in an aircraft') end
local tail = ", " .. SECONDS .. ", " .. tostring(CLEARVIEW) .. ", 0, 1, 1, 30, 1)"
local file = 'getValueResourceByKey("ResKey_Action_902")'
local call
if FORM == 'all' then
  call = 'a_out_picture(' .. file .. tail
elseif FORM == 'coalition' then
  call = 'a_out_picture_s(' .. side .. ', ' .. file .. tail
elseif FORM == 'group' then
  call = 'a_out_picture_g(' .. gid .. ', ' .. file .. tail
else
  call = 'a_out_picture_stop()'
end
local text, flag = net.dostring_in('mission',
  'local ok, err = pcall(function() ' .. call .. ' end) return tostring(ok) .. " " .. tostring(err)')
E.observe('call', call, { info = true })
return tostring(text) .. ' flag=' .. tostring(flag)
