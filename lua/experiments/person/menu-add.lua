-- Person tests: add two F10 radio menu commands for a person to pick by hand:
-- `np-person-all` for everyone (missionCommands.addCommand) and
-- `np-person-group` for the player's group (addCommandForGroup). Each callback
-- counts its runs and records what DCS passed it: the number of arguments and
-- each one's type and value. Then it calls the agent's menu entry under
-- pcall, as the shim's own callback does (lua/experiments/s16/e-menu-client-item.lua).
-- spec/records.md (Layout: MENU) expects only the script's own argument, so
-- the record's `player` is 0 and the sidecar reports AUDIENCE. Read the result
-- with menu-read.lua, which also removes both commands.
-- states: server
-- timeout: 10

local E = ...
local unit
for _, side in ipairs({ coalition.side.RED, coalition.side.BLUE }) do
  unit = unit or (coalition.getPlayers(side) or {})[1]
end
if not unit then
  error('no player in an aircraft')
end
local gid = unit:getGroup():getID()

local function callback(label)
  return function(...)
    local seen = rawget(_G, '__np_person_menu') or {}
    local args = { n = select('#', ...) }
    for i = 1, args.n do
      local v = select(i, ...)
      args[i] = type(v) .. ':' .. (type(v) == 'table' and '{table}' or tostring(v))
    end
    seen[#seen + 1] = label .. ' at ' .. string.format('%.1f', timer.getTime()) .. ' args n=' .. args.n
      .. (args.n > 0 and (' ' .. table.concat(args, ', ')) or '')
    rawset(_G, '__np_person_menu', seen)
    pcall(function()
      rawget(_G, '__nanoprobe_shim').agent.menu('4242', nil, 1)
    end)
  end
end

rawset(_G, '__np_person_menu', {})
rawset(_G, '__np_person_paths', {
  all = missionCommands.addCommand('np-person-all', nil, callback('all'), 'arg-all'),
  group = { gid, missionCommands.addCommandForGroup(gid, 'np-person-group', nil, callback('group'), 'arg-group') },
})
return 'added np-person-all and np-person-group (group ' .. gid .. ')'
