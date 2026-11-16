-- Person tests: read what the callbacks of menu-add.lua recorded, then remove
-- both commands and the globals.
-- states: server
-- timeout: 10

local E = ...
local seen = rawget(_G, '__np_person_menu')
local paths = rawget(_G, '__np_person_paths')
E.observe('callbacks', seen or 'menu-add.lua did not run', { info = true })
if paths then
  pcall(missionCommands.removeItem, paths.all)
  pcall(missionCommands.removeItemForGroup, paths.group[1], paths.group[2])
end
rawset(_G, '__np_person_menu', nil)
rawset(_G, '__np_person_paths', nil)
return seen and #seen or 0
