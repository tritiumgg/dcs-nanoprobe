-- The S16 exit test: select a mission menu command as ED's radio panel does,
-- `missionCommands.doAction(index)` (ED-CODE,
-- Scripts/UI/RadioCommandDialogPanel/RadioCommandDialogsPanel.lua). The
-- command writes the index in place of the placeholder. DCS counts the
-- indices from 0 (LIVE, docs/results/2026-11-15-s16-players-moderation.md), so
-- the caller tries one index after another from 0 until the agent's `MENU`
-- record names its item. Returns `ok`, or `raised <text>`.
-- states: server
-- timeout: 30

local E = ...
local INDEX = tonumber('<index>')
if not INDEX then
  error('run this through cargo xtask commands --part players, which fills in the index')
end
local ok, err = pcall(missionCommands.doAction, INDEX)
if ok then
  return 'ok'
end
return 'raised ' .. tostring(err)
