-- Drawing layers, client check: load the running mission again from its own file,
-- with no player but the server and the one test client, and pause it as soon as
-- the simulation starts. The load removes every drawing; client.lua's place then
-- draws again.
-- states: hook
-- timeout: 300
local E = ...
local file = DCS.getMissionFilename()
local players = #net.get_player_list()
if players > 2 then error('players connected: ' .. players) end
local seq = E.hook.seq()
local ok = net.load_mission(file)
local was
return E.wait(function()
  if was == nil then
    local started = false
    for _, entry in ipairs(E.hook.since(seq)) do
      started = started or entry.name == 'onSimulationStart'
    end
    if not started then return nil end
    was = DCS.getPause()
    if not was then DCS.setPause(true) end
  end
  -- A pause takes effect on a later frame.
  if not DCS.getPause() then return nil end
  return { load = ok, file = DCS.getMissionFilename(), model = DCS.getModelTime(), paused_on_load = was,
    paused = DCS.getPause(), players = #net.get_player_list() }
end, 280)
