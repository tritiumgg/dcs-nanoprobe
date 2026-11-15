-- The S16 exit test (`cargo xtask commands <target> --part players`): the path
-- of the mission DCS has loaded, as DCS opens it. `MISSION_LOAD` takes that
-- path, and only the hook state knows it. Reads only.
-- states: hook
-- timeout: 30

local E = ...
local path = DCS.getMissionFilename()
if type(path) ~= 'string' or path == '' then
  error('DCS reports no mission file')
end
return path
