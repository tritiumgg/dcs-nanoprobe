-- Drawing layers, part C: point the client's F10 map camera straight down at a place,
-- from the client's hook state. run.sh-style placeholders: <x> north, <z> east (DCS
-- map meters) and <h> height (sets the zoom). Uses Export.LoGetCameraPosition and
-- Export.LoSetCameraPosition; returns the position read back.
-- states: hook
-- timeout: 20
local pos = Export.LoGetCameraPosition()
pos.p = { x = tonumber('<x>'), y = tonumber('<h>'), z = tonumber('<z>') }
pos.x = { x = 0, y = -1, z = 0 }
pos.y = { x = 1, y = 0, z = 0 }
pos.z = { x = 0, y = 0, z = 1 }
Export.LoSetCameraPosition(pos)
return { set = pos.p, read = Export.LoGetCameraPosition().p }
