-- E29: whether the gui state of a dedicated server has the Mission Editor
-- modules that could write an empty mission. On the Windows client
-- me_mission is a global in the gui state (LIVE). The rule: a generated
-- template loads on the server and offers slots; otherwise the one-time
-- manual save stays.
-- states: gui hook

local E = ...
E.observe('me_mission_global', type(me_mission), { info = true })
local found = {}
for key in pairs(_G) do
  if type(key) == 'string' and key:find('^me_') then
    found[#found + 1] = key
  end
end
table.sort(found)
E.observe('me_globals', found, { info = true })
local ok, result = pcall(require, 'me_mission')
E.observe('require_me_mission', ok and type(result) or tostring(result):sub(1, 200), { info = true })
E.observe('package_path_has_missioneditor', tostring(package.path):find('MissionEditor', 1, true) ~= nil,
  { info = true })
return 'done'
