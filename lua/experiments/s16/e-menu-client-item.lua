-- Does the agent report a mission menu command chosen through its callback?
-- Run on a server with no players, in the server state, twice:
--   1. `<step>` = `add`: adds the command `np-s16-pick3`. Its callback raises
--      a counter in `_G` and calls `__nanoprobe_shim.agent.menu('4242', nil, 1)`
--      under `pcall`, as the shim's own callback does for an item it holds.
--   2. Choose the command: on the server with `exit-menu-pick.lua`, or as a
--      client through the radio menu.
--   3. `<step>` = `read`: returns the counter, removes the command and the
--      globals.
-- states: server
-- timeout: 30

local E = ...
local STEP = '<step>'
local COUNT = '__np_s16_pick3_count'
local PATH = '__np_s16_pick3_path'

if STEP == 'add' then
  rawset(_G, COUNT, 0)
  local path = missionCommands.addCommand('np-s16-pick3', nil, function()
    rawset(_G, COUNT, (rawget(_G, COUNT) or 0) + 1)
    pcall(function()
      local shim = rawget(_G, '__nanoprobe_shim')
      shim.agent.menu('4242', nil, 1)
    end)
  end)
  rawset(_G, PATH, path)
  return 'added; choose np-s16-pick3, then run the read step'
elseif STEP == 'read' then
  local count = rawget(_G, COUNT)
  local path = rawget(_G, PATH)
  E.observe('callback_count', count, { info = true })
  if path then
    pcall(missionCommands.removeItem, path)
  end
  rawset(_G, COUNT, nil)
  rawset(_G, PATH, nil)
  return count
end
error('fill <step> with add or read')
