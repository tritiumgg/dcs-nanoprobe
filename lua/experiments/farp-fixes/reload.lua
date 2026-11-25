-- FARP fixes: load the running mission again, from its own file, with no
-- player but the server, and pause it at its start. This removed the
-- heliports the earlier snippets left in DCS.
-- Rule: recorded as read.
-- states: hook
-- timeout: 120
local E = ...
local file = DCS.getMissionFilename()
local players = #net.get_player_list()
if players > 1 then error('players connected: ' .. players) end
local ok = net.load_mission(file)
local t0 = os.time()
return E.wait(function()
  if os.time() - t0 < 5 then return nil end
  local m = DCS.getModelTime()
  if m < 5 then
    local was = DCS.getPause()
    if not was then DCS.setPause(true) end
    return { load = ok, file = DCS.getMissionFilename(), model = m, paused_on_load = was, paused = DCS.getPause() }
  end
end, 110)
